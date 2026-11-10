//! Hub tests against a fake sidecar: a thread on the far end of a socket
//! pair that speaks the JSON-lines protocol. No node, no Chromium.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::hub::BrowserHub;
use super::state::TabKey;
use crate::api::schema::{BrowserActor, BrowserOp, BrowserRunParams};
use crate::config::BrowserConfig;

fn pane(id: &str) -> BrowserActor {
    BrowserActor::Pane {
        pane_id: id.into(),
        tab_id: "w2:tD".into(),
        workspace_id: "w2".into(),
        tab_label: "planner".into(),
        workspace_label: None,
        agent: Some("claude".into()),
        session: "default".into(),
        gone: false,
        shell_pid: None,
    }
}

fn params(op: BrowserOp) -> BrowserRunParams {
    BrowserRunParams {
        caller: None,
        profile: None,
        tab: None,
        op,
        timeout_ms: None,
    }
}

/// What the fake host saw and how it should behave.
#[derive(Default)]
pub(crate) struct FakeHostState {
    pub(crate) ops: Vec<(String, Option<String>, Value)>,
    /// Ops the host must never answer.
    pub(crate) silent: Vec<String>,
    /// Ops answered with this error.
    errors: Vec<(String, String, String)>,
    /// Extra lines pushed before the next reply (events).
    events: Vec<String>,
    /// Delay per op name, ms.
    delays: Vec<(String, u64)>,
    next_target: u32,
    /// What `attach` reports about the companion extension (default: ready).
    companion: Option<Value>,
    /// New tab page pushes (`ntp` ops), kept apart so op indices stay stable.
    ntp: Vec<Value>,
    /// The URL act, navigate and history replies report instead of their
    /// defaults (an action that leaves the URL as it was).
    page_url: Option<String>,
}

pub(crate) type Shared = Arc<Mutex<FakeHostState>>;

/// One line per write, under one lock: replies go out from their own
/// threads (some after a delay), and a line that another write splits
/// between its text and its newline is one unparsable line to the hub (its
/// reply lost: the call times out).
fn send_line(writer: &Mutex<UnixStream>, line: &str) {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    let _ = writer.lock().unwrap().write_all(&bytes);
}

pub(crate) fn fake_host(hub: &BrowserHub) -> (Shared, UnixStream) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let shared: Shared = Arc::new(Mutex::new(FakeHostState {
        next_target: 1,
        ..Default::default()
    }));
    let state = shared.clone();
    let writer = Arc::new(Mutex::new(theirs.try_clone().unwrap()));
    let reader = BufReader::new(theirs);
    std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let request: Value = serde_json::from_str(&line).unwrap();
            let op = request["op"].as_str().unwrap().to_string();
            let target = request["target"].as_str().map(str::to_string);
            let id = request["id"].as_u64().unwrap();
            let mut args = request["args"].clone();
            // The envelope's activity directive, kept with the args for assertions.
            if let Some(activity) = request.get("activity") {
                if args.is_null() {
                    args = json!({});
                }
                args["_activity"] = activity.clone();
            }
            let (reply, events, delay) = {
                let mut state = state.lock().unwrap();
                if op == "ntp" {
                    state.ntp.push(args.clone());
                } else {
                    state.ops.push((op.clone(), target.clone(), args.clone()));
                }
                let mut events: Vec<String> = state.events.drain(..).collect();
                let delay = state
                    .delays
                    .iter()
                    .find(|(name, _)| name == &op)
                    .map(|(_, ms)| *ms)
                    .unwrap_or(0);
                if state.silent.contains(&op) {
                    (None, events, delay)
                } else if let Some((_, code, message)) =
                    state.errors.iter().find(|(name, _, _)| name == &op)
                {
                    // A failed open still made a tab: the error names it, like the sidecar's.
                    let mut error = json!({ "code": code, "message": message });
                    if op == "open" {
                        let n = state.next_target;
                        state.next_target += 1;
                        let target = format!("T{n}");
                        events.push(json!({ "event": "tab", "profile": "main", "target": target, "kind": "opened", "url": "about:blank", "initiator": "other" }).to_string());
                        error["target"] = json!(target);
                    }
                    (
                        Some(json!({ "id": id, "ok": false, "error": error })),
                        events,
                        delay,
                    )
                } else {
                    let page = json!({ "url": format!("https://site.test/{}", target.clone().unwrap_or_default()), "title": "Site", "dialog_open": false, "status": 200 });
                    let reply = match op.as_str() {
                        "hello" => {
                            json!({ "id": id, "ok": true, "result": { "host_protocol": 1, "playwright_version": "1.63.0", "node_version": "v22" } })
                        }
                        "ping" => json!({ "id": id, "ok": true, "result": {} }),
                        "attach" => {
                            let companion = state
                                .companion
                                .clone()
                                .unwrap_or_else(|| json!({ "state": "ready", "detail": "" }));
                            json!({ "id": id, "ok": true, "result": { "tabs": [ { "target": "U1", "url": "https://user.test/", "title": "User tab", "selected": true } ], "companion": companion } })
                        }
                        "tabs" => {
                            json!({ "id": id, "ok": true, "result": { "tabs": [ { "target": "U1", "url": "https://user.test/", "title": "User tab", "selected": true } ] } })
                        }
                        "open" => {
                            let n = state.next_target;
                            state.next_target += 1;
                            let target = format!("T{n}");
                            // The real sidecar's `opened` event (initiator other: the
                            // page event fires before the reply) reaches the ledger first.
                            let profile = request["profile"].as_str().unwrap_or("main");
                            events.push(json!({ "event": "tab", "profile": profile, "target": target, "kind": "opened", "url": "about:blank", "initiator": "other" }).to_string());
                            json!({ "id": id, "ok": true, "result": { "target": target, "card": { "headings": 2, "links": 5, "forms": 0, "text_chars": 900, "main_chars": 400, "login_wall": false, "preview": "# Opened" } }, "page": { "url": args["url"], "title": "Opened", "status": 200 } })
                        }
                        "read" => {
                            let content: String = (0..500)
                                .map(|i| char::from(b'a' + (i % 26) as u8))
                                .collect();
                            json!({ "id": id, "ok": true, "result": { "content": content, "total_chars": 500 }, "page": page })
                        }
                        "links" => {
                            json!({ "id": id, "ok": true, "result": { "links": [ { "text": "Docs", "href": "https://site.test/docs" } ], "total": 1 }, "page": page })
                        }
                        "screenshot" => {
                            json!({ "id": id, "ok": true, "result": { "path": args["path"], "inline_path": null, "width": 1200, "height": 800 }, "page": page })
                        }
                        "console" => {
                            json!({ "id": id, "ok": true, "result": { "entries": [ { "seq": 1, "level": "error", "text": "boom", "url": "https://site.test/app.js", "line": 3 } ], "next_seq": 1 }, "page": page })
                        }
                        "network" => {
                            json!({ "id": id, "ok": true, "result": { "entries": [], "next_seq": 0 }, "page": page })
                        }
                        "wait" => {
                            json!({ "id": id, "ok": true, "result": { "matched": true, "elapsed_ms": 12 }, "page": page })
                        }
                        "scroll" => {
                            json!({ "id": id, "ok": true, "result": { "scroll_y": 100, "scroll_height": 3000 }, "page": page })
                        }
                        "eval" => {
                            // The guard refuses code that touches the fixture's password field.
                            let guarded = args["guard_passwords"].as_bool().unwrap_or(false);
                            if guarded && args["expr"].as_str().is_some_and(|e| e.contains("#pw")) {
                                json!({ "id": id, "ok": false, "error": { "code": "password_field_refused", "message": "eval changed a password field; restored" } })
                            } else {
                                json!({ "id": id, "ok": true, "result": { "value": { "a": 1 } }, "page": page })
                            }
                        }
                        "dialog" => {
                            json!({ "id": id, "ok": true, "result": { "type": "alert", "message": "hi" }, "page": page })
                        }
                        "act" => {
                            let kind = args["kind"].as_str().unwrap_or("").to_string();
                            let is_pw = args["ref"].as_str() == Some("e16")
                                || args["selector"].as_str() == Some("#pw");
                            if is_pw
                                && !args["allow_password"].as_bool().unwrap_or(false)
                                && kind != "click"
                            {
                                json!({ "id": id, "ok": false, "error": { "code": "password_field_refused", "message": "that is a password field" } })
                            } else if args["ref"].as_str() == Some("e99") {
                                json!({ "id": id, "ok": false, "error": { "code": "stale_ref", "message": "ref e99 no longer resolves; re-run browser snapshot" } })
                            } else {
                                json!({ "id": id, "ok": true, "result": { "kind": kind, "role": if kind == "click" { "button" } else { "textbox" }, "name": if kind == "click" { "Press me" } else { "Name" }, "navigated": kind == "click", "url_before": "https://site.test/T1" }, "page": { "url": state.page_url.clone().unwrap_or_else(|| "https://site.test/after".into()), "title": "After", "dialog_open": false } })
                            }
                        }
                        "navigate" | "history" | "focus" | "select" => {
                            json!({ "id": id, "ok": true, "result": {}, "page": { "url": state.page_url.clone().unwrap_or_else(|| args["url"].as_str().unwrap_or("https://site.test/after").to_string()), "title": "After", "status": 200 } })
                        }
                        "close" | "close_browser" | "detach" | "watch" | "unwatch"
                        | "control_quit" | "control_raise" => {
                            json!({ "id": id, "ok": true, "result": {} })
                        }
                        // Fork (companion browser): an HTML view.
                        "show" => {
                            let n = state.next_target;
                            state.next_target += 1;
                            let target = format!("T{n}");
                            let file = args["file"].as_str().unwrap_or_default().to_string();
                            json!({ "id": id, "ok": true, "result": { "target": target, "view_id": format!("v{n}"), "watching": args["watch"] }, "page": { "url": format!("file://{file}"), "title": "View", "status": 200 } })
                        }
                        "ntp" => json!({ "id": id, "ok": true, "result": { "pushed": 1 } }),
                        "dashboard" => {
                            json!({ "id": id, "ok": true, "result": { "windows": 1, "pin": args["pin"] } })
                        }
                        "release" => {
                            // Keys with "fail" in them are reported back as failed.
                            let keys: Vec<String> = args["keys"]
                                .as_array()
                                .map(|k| {
                                    k.iter()
                                        .filter_map(|v| v.as_str().map(str::to_string))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let (failed, released): (Vec<String>, Vec<String>) =
                                keys.into_iter().partition(|k| k.contains("fail"));
                            json!({ "id": id, "ok": true, "result": { "released": released, "failed": failed, "reason": "companion not ready" } })
                        }
                        other => {
                            json!({ "id": id, "ok": false, "error": { "code": "unknown_op", "message": other } })
                        }
                    };
                    (Some(reply), events, delay)
                }
            };
            for event in events {
                send_line(&writer, &event);
            }
            if let Some(reply) = reply {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    if delay > 0 {
                        std::thread::sleep(Duration::from_millis(delay));
                    }
                    send_line(&writer, &reply.to_string());
                });
            }
        }
    });
    let reader: Box<dyn BufRead + Send> = Box::new(BufReader::new(ours.try_clone().unwrap()));
    hub.install_fake_host(Box::new(ours.try_clone().unwrap()), reader);
    (shared, ours)
}

/// Fork (companion launcher): these tests drive the herdr+ window, which
/// is off by default since the companion launcher: they turn it on, with
/// the old `auto` host (companion while leased, else the window).
pub(crate) fn window_config() -> BrowserConfig {
    BrowserConfig {
        window: true,
        host: "auto".into(),
        ..BrowserConfig::default()
    }
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("herdr-browser-hub-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn hub(name: &str) -> (BrowserHub, Shared, UnixStream, PathBuf) {
    let home = temp_home(name);
    let hub = BrowserHub::with_home(window_config(), home.clone());
    hub.set_profile_running_for_test("main", 0, 4321);
    let (shared, stream) = fake_host(&hub);
    (hub, shared, stream, home)
}

#[test]
fn open_then_read_uses_the_cursor_and_records_attribution() {
    let (hub, shared, _stream, home) = hub("open-read");
    let data_dir = home.join("session");
    hub.enable_persistence(&data_dir);
    let actor = pane("w2:pD");
    let opened = hub
        .run(
            &actor,
            params(BrowserOp::Open {
                url: "site.test/start".into(),
                focus: false,
                wait: None,
            }),
        )
        .unwrap();
    assert_eq!(
        opened.tab.as_deref(),
        Some("main:t2"),
        "the user's attached tab took t1"
    );
    assert!(
        opened
            .header
            .starts_with("[main:t2 · 200 · site.test/start · \"Opened\"]"),
        "{}",
        opened.header
    );
    assert!(opened.text.contains("headings 2 · links 5"));
    assert!(opened.text.contains("# Opened"));
    let attach_args = shared.lock().unwrap().ops[0].clone();
    assert_eq!(attach_args.0, "attach");
    assert_eq!(attach_args.2["port"], 4321);
    let open_args = shared.lock().unwrap().ops[1].clone();
    assert_eq!(open_args.2["url"], "https://site.test/start");
    assert_eq!(open_args.2["background"], true);

    let read = hub
        .run(
            &actor,
            params(BrowserOp::Read {
                format: None,
                selector: None,
                ref_: None,
                offset: None,
                max: Some(200),
                all: false,
                interactive: false,
            }),
        )
        .unwrap();
    assert_eq!(read.tab.as_deref(), Some("main:t2"));
    assert!(
        read.text
            .ends_with("[chars 0–200 of 500 · next: browser read t2 --offset 200]"),
        "{}",
        read.text
    );
    assert_eq!(read.data["next_offset"], 200);
    let (_, target, args) = shared.lock().unwrap().ops[2].clone();
    assert_eq!(target.as_deref(), Some("T1"));
    assert_eq!(args["format"], "markdown");

    hub.with_state(|state| {
        let key = TabKey::new("main", "T1");
        let record = &state.tabs[&key];
        assert_eq!(record.opened_by.pane_id(), Some("w2:pD"));
        assert_eq!(record.last.as_ref().unwrap().op, "read");
        assert_eq!(record.last.as_ref().unwrap().detail, "markdown 0–200/500");
        assert_eq!(record.users, ["w2:pD"]);
        assert_eq!(state.cursor("w2:pD").unwrap().target_id, "T1");
        let user_tab = &state.tabs[&TabKey::new("main", "U1")];
        assert!(user_tab.opened_by.is_user());
        assert_eq!(user_tab.short, "t1");
        let log = state.activity(10, None, None);
        assert_eq!(log[0].op, "read");
        assert_eq!(log[1].op, "open");
        assert_eq!(log[1].detail, "site.test/start");
    });
    let info = hub.get(None);
    assert_eq!(info.profiles[0].tabs, 2);
    assert_eq!(info.profiles[0].agents, 1);
    assert_eq!(info.recent_panes[0].current, "main:t2");
    assert!(hub.get(Some(info.seq)).unchanged);

    // persisted, and a fresh hub loads the ids back
    assert!(data_dir.join("browser.json").exists());
    assert_eq!(
        std::fs::read_to_string(data_dir.join("browser-activity.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let again = BrowserHub::with_home(window_config(), home.clone());
    again.enable_persistence(&data_dir);
    again.with_state(|state| {
        assert_eq!(state.tabs[&TabKey::new("main", "T1")].short, "t2");
        assert!(
            !state.tabs[&TabKey::new("main", "T1")].is_open(),
            "closed until reconciled"
        );
    });
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn interleaved_replies_from_two_panes_are_correlated() {
    let (hub, shared, _stream, home) = hub("interleave");
    shared.lock().unwrap().delays.push(("read".into(), 300));
    let a = pane("w2:pA");
    let b = pane("w2:pB");
    hub.run(
        &a,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    hub.run(
        &b,
        params(BrowserOp::Open {
            url: "https://b.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let hub_a = hub.clone();
    let ta = std::thread::spawn(move || {
        hub_a
            .run(
                &a,
                params(BrowserOp::Read {
                    format: None,
                    selector: None,
                    ref_: None,
                    offset: None,
                    max: None,
                    all: true,
                    interactive: false,
                }),
            )
            .unwrap()
    });
    let hub_b = hub.clone();
    let tb = std::thread::spawn(move || {
        hub_b
            .run(
                &b,
                params(BrowserOp::Links {
                    filter: None,
                    max: None,
                }),
            )
            .unwrap()
    });
    let ra = ta.join().unwrap();
    let rb = tb.join().unwrap();
    assert_eq!(ra.tab.as_deref(), Some("main:t2"));
    assert!(ra.data["content"].as_str().unwrap().len() == 500);
    assert_eq!(rb.tab.as_deref(), Some("main:t3"));
    assert_eq!(rb.data["links"][0]["text"], "Docs");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_silent_op_times_out_without_dropping_the_host() {
    let (hub, shared, _stream, home) = hub("timeout");
    shared.lock().unwrap().silent.push("scroll".into());
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let mut p = params(BrowserOp::Scroll {
        to: Some("top".into()),
        by: None,
    });
    p.timeout_ms = Some(1);
    let started = std::time::Instant::now();
    let err = hub.run(&actor, p).unwrap_err();
    assert_eq!(err.code, "browser_timeout");
    assert!(started.elapsed() >= super::hub::REPLY_GRACE);
    assert!(hub.has_host(), "a slow op is not a wedged sidecar");
    // the next op still works and the failure is in the log
    let ok = hub
        .run(
            &actor,
            params(BrowserOp::Eval {
                expr: "1".into(),
                max: None,
            }),
        )
        .unwrap();
    assert_eq!(ok.data["value"]["a"], 1);
    hub.with_state(|state| {
        let log = state.activity(5, None, None);
        assert_eq!(log[1].op, "scroll");
        assert!(!log[1].ok);
        assert!(log[1].detail.starts_with("browser_timeout"));
    });
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn host_errors_map_to_codes_and_a_closed_tab_clears_the_cursor() {
    let (hub, shared, _stream, home) = hub("errors");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    shared.lock().unwrap().errors.push((
        "read".into(),
        "stale_ref".into(),
        "ref e9 is gone".into(),
    ));
    let err = hub
        .run(
            &actor,
            params(BrowserOp::Read {
                format: None,
                selector: None,
                ref_: Some("e9".into()),
                offset: None,
                max: None,
                all: false,
                interactive: false,
            }),
        )
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        ("stale_ref", "ref e9 is gone")
    );
    // the sidecar reports the user closed the tab
    shared.lock().unwrap().events.push(
        json!({ "event": "tab", "profile": "main", "target": "T1", "kind": "closed" }).to_string(),
    );
    let _ = hub.run(&actor, params(BrowserOp::Tabs { mine: false }));
    std::thread::sleep(Duration::from_millis(100));
    let err = hub
        .run(
            &actor,
            params(BrowserOp::Links {
                filter: None,
                max: None,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "no_current_tab");
    assert!(err.message.contains("browser open"));
    hub.with_state(|state| assert!(!state.tabs[&TabKey::new("main", "T1")].is_open()));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn explicit_tab_moves_the_cursor_and_unknown_tabs_error() {
    let (hub, _shared, _stream, home) = hub("explicit");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let mut p = params(BrowserOp::Read {
        format: Some("text".into()),
        selector: None,
        ref_: None,
        offset: None,
        max: None,
        all: true,
        interactive: false,
    });
    p.tab = Some("t1".into()); // the user's tab
    let read = hub.run(&actor, p).unwrap();
    assert_eq!(read.tab.as_deref(), Some("main:t1"));
    hub.with_state(|state| assert_eq!(state.cursor("w2:pD").unwrap().target_id, "U1"));
    let mut p = params(BrowserOp::Read {
        format: None,
        selector: None,
        ref_: None,
        offset: None,
        max: None,
        all: true,
        interactive: false,
    });
    p.tab = Some("t9".into());
    assert_eq!(hub.run(&actor, p).unwrap_err().code, "tab_not_found");
    let mut p = params(BrowserOp::Use);
    p.tab = Some("main:t2".into());
    p.caller = None;
    let used = hub.run(&actor, p).unwrap();
    assert!(used.text.contains("current tab is now main:t2"));
    hub.with_state(|state| assert_eq!(state.cursor("w2:pD").unwrap().target_id, "T1"));
    let err = hub
        .run(
            &actor,
            params(BrowserOp::History {
                action: "sideways".into(),
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "invalid_request");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn config_switches_are_honoured() {
    let (hub, _shared, _stream, home) = hub("config");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    hub.apply_config(&BrowserConfig {
        allow_eval: false,
        ..window_config()
    });
    assert_eq!(
        hub.run(
            &actor,
            params(BrowserOp::Eval {
                expr: "1".into(),
                max: None
            })
        )
        .unwrap_err()
        .code,
        "eval_disabled"
    );
    hub.apply_config(&BrowserConfig {
        enabled: false,
        ..window_config()
    });
    assert_eq!(
        hub.run(&actor, params(BrowserOp::Tabs { mine: false }))
            .unwrap_err()
            .code,
        "browser_disabled"
    );
    assert!(!hub.get(None).enabled);
    hub.apply_config(&window_config());
    assert_eq!(
        hub.run(&BrowserActor::User, params(BrowserOp::Unknown))
            .unwrap_err()
            .code,
        "unknown_op"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn external_callers_have_no_cursor_and_the_host_going_away_fails_pending_calls() {
    let (hub, shared, stream, home) = hub("external");
    let external = BrowserActor::External { raw: None };
    let opened = hub
        .run(
            &external,
            params(BrowserOp::Open {
                url: "https://a.test/".into(),
                focus: false,
                wait: None,
            }),
        )
        .unwrap();
    assert_eq!(opened.tab.as_deref(), Some("main:t2"));
    assert_eq!(
        hub.run(
            &external,
            params(BrowserOp::Links {
                filter: None,
                max: None
            })
        )
        .unwrap_err()
        .code,
        "no_current_tab"
    );
    hub.with_state(|state| assert!(state.tabs[&TabKey::new("main", "T1")].users.is_empty()));
    shared.lock().unwrap().silent.push("console".into());
    let hub2 = hub.clone();
    let t = std::thread::spawn(move || {
        let mut p = params(BrowserOp::Console {
            level: None,
            since: None,
            max: None,
        });
        p.tab = Some("t2".into());
        hub2.run(&external, p)
    });
    std::thread::sleep(Duration::from_millis(200));
    stream.shutdown(std::net::Shutdown::Both).unwrap();
    stream_gone(&hub);
    let err = t.join().unwrap().unwrap_err();
    assert_eq!(err.code, "browser_host_restarted");
    assert!(!hub.has_host());
    let _ = std::fs::remove_dir_all(&home);
}

fn stream_gone(hub: &BrowserHub) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while hub.has_host() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn profiles_new_is_a_fresh_temporary_and_protected_ones_refuse_delete() {
    let (hub, _shared, _stream, home) = hub("profiles");
    let created = hub.profile_create("new", true).unwrap();
    assert!(created.name.starts_with("tmp-"));
    assert!(created.temporary);
    assert!(created.exists);
    assert!(matches!(hub.profile_delete("main"), Err(err) if err.code == "profile_protected"));
    hub.set_profile_running_for_test(&created.name, 0, 1);
    assert!(matches!(hub.profile_delete(&created.name), Err(err) if err.code == "profile_running"));
    assert!(
        matches!(hub.profile_create("Bad Name", false), Err(err) if err.code == "invalid_profile")
    );
    let names: Vec<String> = hub.profiles().into_iter().map(|p| p.name).collect();
    assert!(names.contains(&"main".to_string()));
    assert!(names.contains(&created.name));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_qualified_tab_selects_its_profile_and_disagreeing_flags_are_refused() {
    let (hub, shared, _stream, home) = hub("qualified");
    hub.set_profile_running_for_test("work", 0, 4322);
    let actor = pane("w2:pD");
    let mut p = params(BrowserOp::Open {
        url: "https://w.test/".into(),
        focus: false,
        wait: None,
    });
    p.profile = Some("work".into());
    let opened = hub.run(&actor, p).unwrap();
    assert_eq!(opened.tab.as_deref(), Some("work:t2"));
    // another pane, cursor on main, reads the work tab by its qualified id
    let other = pane("w2:pE");
    hub.run(
        &other,
        params(BrowserOp::Open {
            url: "https://m.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let mut p = params(BrowserOp::Links {
        filter: None,
        max: None,
    });
    p.tab = Some("work:t2".into());
    let links = hub.run(&other, p).unwrap();
    assert_eq!(links.tab.as_deref(), Some("work:t2"));
    let (_, target, _) = shared.lock().unwrap().ops.last().cloned().unwrap();
    assert_eq!(target.as_deref(), Some("T1"), "the work tab's target");
    let mut p = params(BrowserOp::Links {
        filter: None,
        max: None,
    });
    p.tab = Some("work:t2".into());
    p.profile = Some("main".into());
    let err = hub.run(&other, p).unwrap_err();
    assert_eq!(err.code, "invalid_request");
    assert!(err.message.contains("disagrees"));
    let mut p = params(BrowserOp::Links {
        filter: None,
        max: None,
    });
    p.tab = Some("Bad Profile:t2".into());
    assert_eq!(hub.run(&other, p).unwrap_err().code, "invalid_request");
    // use without a tab is refused; with a qualified tab it moves the cursor
    assert_eq!(
        hub.run(&other, params(BrowserOp::Use)).unwrap_err().code,
        "invalid_request"
    );
    let mut p = params(BrowserOp::Use);
    p.tab = Some("work:t2".into());
    hub.run(&other, p).unwrap();
    hub.with_state(|state| assert_eq!(state.cursor_profile("w2:pE"), Some("work")));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn profile_delete_refuses_starting_in_use_live_lock_and_busy() {
    let (hub, _shared, _stream, home) = hub("delete-guards");
    let store = hub.profiles_store_for_test();
    store.ensure("scratch", true, 1).unwrap();
    hub.with_state_mut(|state| {
        state.set_profile(
            "scratch",
            super::state::ProfileStatus::Starting { since: 1 },
        )
    });
    assert_eq!(
        hub.profile_delete("scratch").unwrap_err().code,
        "profile_running"
    );
    hub.with_state_mut(|state| {
        state.set_profile(
            "scratch",
            super::state::ProfileStatus::InUse {
                pid: Some(1),
                at: 1,
            },
        )
    });
    assert_eq!(
        hub.profile_delete("scratch").unwrap_err().code,
        "profile_running"
    );
    hub.with_state_mut(|state| state.set_profile("scratch", super::state::ProfileStatus::Stopped));
    // a live SingletonLock pid herdr did not record, with a listening
    // SingletonSocket
    std::os::unix::fs::symlink(
        format!("host-{}", std::process::id()),
        store.dir("scratch").join("SingletonLock"),
    )
    .unwrap();
    let socket = std::env::temp_dir().join(format!("herdr-pd-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::os::unix::fs::symlink(&socket, store.dir("scratch").join("SingletonSocket")).unwrap();
    let err = hub.profile_delete("scratch").unwrap_err();
    assert_eq!(err.code, "profile_running");
    assert!(err.message.contains("still holds"));
    std::fs::remove_file(store.dir("scratch").join("SingletonSocket")).unwrap();
    drop(listener);
    let _ = std::fs::remove_file(&socket);
    std::fs::remove_file(store.dir("scratch").join("SingletonLock")).unwrap();
    // a run record with a live pid whose DevTools answers: the browser
    // herdr launched
    let devtools = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let devtools_port = devtools.local_addr().unwrap().port();
    let answering = std::thread::spawn(move || {
        let (mut stream, _) = devtools.accept().unwrap();
        let mut buf = [0u8; 1024];
        let _ = std::io::Read::read(&mut stream, &mut buf);
        let body = r#"{"Browser":"Chrome/154.0.8037.93"}"#;
        let _ = std::io::Write::write_all(
            &mut stream,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    });
    super::launch::write_run_record(
        hub_home(&hub),
        "scratch",
        &super::launch::RunRecord {
            pid: std::process::id(),
            port: devtools_port,
            exe: String::new(),
            launched_at: 0,
            server_pid: 0,
            argv: vec![],
            browser: String::new(),
        },
    )
    .unwrap();
    assert_eq!(
        hub.profile_delete("scratch").unwrap_err().code,
        "profile_running"
    );
    answering.join().unwrap();
    super::launch::clear_run_record(hub_home(&hub), "scratch");
    // the profile lock held by a lifecycle op: busy, never a blocking wait
    let lock = hub.profile_lock_for_test("scratch");
    let guard = lock.lock().unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        hub.profile_delete("scratch").unwrap_err().code,
        "profile_busy"
    );
    assert!(started.elapsed() < Duration::from_millis(200));
    drop(guard);
    // a stale run record: its pid alive (reused) but no DevTools answers,
    // and no lock: not a holder, the delete goes through
    super::launch::write_run_record(
        hub_home(&hub),
        "scratch",
        &super::launch::RunRecord {
            pid: std::process::id(),
            port: 1,
            exe: String::new(),
            launched_at: 0,
            server_pid: 0,
            argv: vec![],
            browser: String::new(),
        },
    )
    .unwrap();
    hub.profile_delete("scratch").unwrap();
    assert!(!store.exists("scratch"));
    let _ = std::fs::remove_dir_all(&home);
}

fn hub_home(hub: &BrowserHub) -> &std::path::Path {
    hub.home_for_test()
}

#[test]
fn concurrent_flushes_never_duplicate_activity_lines() {
    let (hub, _shared, _stream, home) = hub("flush");
    let data_dir = home.join("session");
    hub.enable_persistence(&data_dir);
    let key = TabKey::new("main", "F");
    hub.with_state_mut(|state| {
        state.adopt_tab(
            &key,
            &super::state::HostTab {
                target: "F".into(),
                ..Default::default()
            },
            &BrowserActor::User,
            1,
        )
    });
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let hub = hub.clone();
            let key = key.clone();
            std::thread::spawn(move || {
                for j in 0..25 {
                    hub.with_state_mut(|state| {
                        state.touch(
                            "main",
                            Some(&key),
                            &pane(&format!("p{i}")),
                            "eval",
                            &format!("{j}"),
                            true,
                            0,
                            2,
                        )
                    });
                    hub.flush();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    hub.flush();
    let lines = std::fs::read_to_string(data_dir.join("browser-activity.jsonl")).unwrap();
    let seqs: Vec<u64> = lines
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(seqs.len(), 200, "every touch once");
    let mut dedup = seqs.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(dedup.len(), seqs.len(), "no duplicate lines");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn paging_reuses_the_last_extraction_and_a_fresh_read_re_extracts() {
    let (hub, shared, _stream, home) = hub("read-cache");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let reads = |shared: &Shared| {
        shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|(op, _, _)| op == "read")
            .count()
    };
    let read = |offset: u64| {
        params(BrowserOp::Read {
            format: None,
            selector: None,
            ref_: None,
            offset: Some(offset),
            max: Some(100),
            all: false,
            interactive: false,
        })
    };
    let first = hub.run(&actor, read(0)).unwrap();
    assert_eq!(reads(&shared), 1);
    let second = hub.run(&actor, read(100)).unwrap();
    assert_eq!(reads(&shared), 1, "paging reuses the extraction");
    assert_eq!(second.data["offset"], 100);
    assert_ne!(first.data["content"], second.data["content"]);
    // find on the same page reuses the markdown too
    hub.run(
        &actor,
        params(BrowserOp::Find {
            query: "abc".into(),
            max: None,
            context: None,
        }),
    )
    .unwrap();
    assert_eq!(reads(&shared), 1);
    // a fresh read at offset 0 re-extracts; a different format too
    hub.run(&actor, read(0)).unwrap();
    assert_eq!(reads(&shared), 2);
    let mut text = read(50);
    if let BrowserOp::Read { format, .. } = &mut text.op {
        *format = Some("text".into());
    }
    hub.run(&actor, text).unwrap();
    assert_eq!(reads(&shared), 3, "another format is another extraction");
    // a navigation changes the URL: the cache no longer matches
    hub.run(
        &actor,
        params(BrowserOp::Navigate {
            url: "https://b.test/".into(),
            wait: None,
        }),
    )
    .unwrap();
    hub.run(&actor, read(100)).unwrap();
    assert_eq!(reads(&shared), 4);
    let _ = std::fs::remove_dir_all(&home);
}

fn read_ops(shared: &Shared) -> usize {
    shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .filter(|(op, _, _)| op == "read")
        .count()
}

fn read_at(offset: u64) -> BrowserRunParams {
    params(BrowserOp::Read {
        format: None,
        selector: None,
        ref_: None,
        offset: Some(offset),
        max: Some(100),
        all: false,
        interactive: false,
    })
}

fn find_op(query: &str) -> BrowserRunParams {
    params(BrowserOp::Find {
        query: query.into(),
        max: None,
        context: None,
    })
}

fn click_op() -> BrowserRunParams {
    params(BrowserOp::Click {
        ref_: Some("e5".into()),
        selector: None,
    })
}

#[test]
fn fork_smoke_a_same_url_navigate_or_a_click_drops_the_cached_read() {
    let (hub, shared, _stream, home) = hub("read-cache-actions");
    let actor = pane("w2:pD");
    // every action here leaves the URL as the read saw it
    shared.lock().unwrap().page_url = Some("https://site.test/T1".into());
    hub.run(&actor, params(open_op("https://a.test/"))).unwrap();
    hub.run(&actor, read_at(0)).unwrap();
    let cached = hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 1, "paging right after a read reuses it");
    assert_eq!(cached.data["cached"], true);
    assert!(cached.data["age_ms"].is_u64());
    assert!(cached.text.contains("[cached copy "), "{}", cached.text);
    // a navigation to the very URL the tab shows (a reload, a rebase's
    // refresh) still drops the copy
    let url = hub.with_state(|state| state.tabs[&TabKey::new("main", "T1")].url.clone());
    hub.run(
        &actor,
        params(BrowserOp::Navigate {
            url: url.clone(),
            wait: None,
        }),
    )
    .unwrap();
    assert_eq!(
        hub.with_state(|state| state.tabs[&TabKey::new("main", "T1")].url.clone()),
        url
    );
    let fresh = hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 2, "a same-URL navigate re-extracts");
    assert!(fresh.data.get("cached").is_none());
    assert!(!fresh.text.contains("cached copy"));
    hub.run(
        &actor,
        params(BrowserOp::History {
            action: "reload".into(),
        }),
    )
    .unwrap();
    hub.run(&actor, find_op("abc")).unwrap();
    assert_eq!(read_ops(&shared), 3, "a reload: find re-extracts");
    // a click (an in-page modal, a merge) drops it too: paging and find
    hub.run(&actor, read_at(0)).unwrap();
    assert_eq!(read_ops(&shared), 4);
    hub.run(&actor, click_op()).unwrap();
    hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 5, "a click: paging re-extracts");
    hub.run(&actor, click_op()).unwrap();
    hub.run(&actor, find_op("abc")).unwrap();
    assert_eq!(read_ops(&shared), 6, "a click: find re-extracts");
    // a failed action counts as well (it may have half-happened)
    hub.run(&actor, read_at(0)).unwrap();
    shared.lock().unwrap().errors.push((
        "act".into(),
        "action_timeout".into(),
        "click did not complete".into(),
    ));
    assert!(hub.run(&actor, click_op()).is_err());
    hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 8);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fork_smoke_find_re_extracts_unless_it_directly_follows_a_fresh_read() {
    let (hub, shared, _stream, home) = hub("read-cache-find");
    let actor = pane("w2:pD");
    hub.run(&actor, params(open_op("https://a.test/"))).unwrap();
    hub.run(&actor, find_op("abc")).unwrap();
    hub.run(&actor, find_op("abc")).unwrap();
    assert_eq!(
        read_ops(&shared),
        2,
        "a find never reuses another find's copy"
    );
    // paging around a match reuses the find's extraction (same offsets)
    hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 2);
    hub.run(&actor, read_at(0)).unwrap();
    let hit = hub.run(&actor, find_op("abc")).unwrap();
    assert_eq!(read_ops(&shared), 3, "find right after a read reuses it");
    assert_eq!(hit.data["cached"], true);
    assert!(hit.text.contains("[cached copy "), "{}", hit.text);
    // read → find → find: only the first find reuses the read's copy
    let again = hub.run(&actor, find_op("abc")).unwrap();
    assert_eq!(
        read_ops(&shared),
        4,
        "a second find after a read re-extracts"
    );
    assert!(again.data.get("cached").is_none());
    // an old copy is not reused, for find or for paging
    hub.run(&actor, read_at(0)).unwrap();
    hub.age_read_cache_for_test(Duration::from_secs(6));
    let miss = hub.run(&actor, find_op("abc")).unwrap();
    assert_eq!(read_ops(&shared), 6);
    assert!(miss.data.get("cached").is_none());
    hub.age_read_cache_for_test(Duration::from_secs(6));
    hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 7);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fork_smoke_a_cached_read_writes_no_old_page_back_and_respects_an_open_dialog() {
    let (hub, shared, _stream, home) = hub("read-cache-page");
    let actor = pane("w2:pD");
    let key = TabKey::new("main", "T1");
    hub.run(&actor, params(open_op("https://a.test/"))).unwrap();
    hub.run(&actor, read_at(0)).unwrap();
    // the page renames itself; `focus` keeps the copy and brings the event in
    shared.lock().unwrap().events.push(
        json!({ "event": "tab", "profile": "main", "target": "T1", "kind": "title", "title": "Renamed" }).to_string(),
    );
    hub.run(&actor, params(BrowserOp::Focus)).unwrap();
    assert_eq!(
        hub.with_state(|state| state.tabs[&key].title.clone()),
        "Renamed"
    );
    let cached = hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 1);
    assert_eq!(
        hub.with_state(|state| state.tabs[&key].title.clone()),
        "Renamed",
        "the cached copy's page is not written back"
    );
    assert!(cached.header.contains("Renamed"), "{}", cached.header);
    // a dialog opened: the read goes to the sidecar (its guard answers)
    shared.lock().unwrap().events.push(
        json!({ "event": "dialog", "profile": "main", "target": "T1", "type": "alert", "message": "hi", "state": "open" }).to_string(),
    );
    hub.run(&actor, params(BrowserOp::Focus)).unwrap();
    assert!(hub.with_state(|state| state.tabs[&key].dialog_open));
    hub.run(&actor, read_at(100)).unwrap();
    assert_eq!(read_ops(&shared), 2, "no cached read past an open dialog");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn act_ops_log_the_element_never_the_text_and_honour_the_switches() {
    let (hub, shared, _stream, home) = hub("act");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let typed = hub
        .run(
            &actor,
            params(BrowserOp::Type {
                ref_: Some("e14".into()),
                selector: None,
                text: "secret text".into(),
                submit: true,
                clear: false,
            }),
        )
        .unwrap();
    assert!(
        typed
            .text
            .starts_with("typed 11 chars into textbox \"Name\" (e14)"),
        "{}",
        typed.text
    );
    let (_, _, args) = shared.lock().unwrap().ops.last().cloned().unwrap();
    assert_eq!(args["kind"], "type");
    assert_eq!(args["text"], "secret text", "the sidecar gets the text");
    assert_eq!(args["allow_password"], false);
    let clicked = hub
        .run(
            &actor,
            params(BrowserOp::Click {
                ref_: Some("e11".into()),
                selector: None,
            }),
        )
        .unwrap();
    assert!(
        clicked.text.contains("clicked button \"Press me\" (e11)"),
        "{}",
        clicked.text
    );
    assert!(
        clicked
            .text
            .contains("navigated: site.test/T1 → site.test/after"),
        "{}",
        clicked.text
    );
    let refused = hub
        .run(
            &actor,
            params(BrowserOp::Fill {
                ref_: Some("e16".into()),
                selector: None,
                text: "hunter2".into(),
            }),
        )
        .unwrap_err();
    assert_eq!(refused.code, "password_field_refused");
    let stale = hub
        .run(
            &actor,
            params(BrowserOp::Hover {
                ref_: Some("e99".into()),
                selector: None,
            }),
        )
        .unwrap_err();
    assert_eq!(stale.code, "stale_ref");
    hub.with_state(|state| {
        let log = state.activity(10, None, None);
        assert_eq!(log[0].op, "act:hover");
        assert!(!log[0].ok);
        assert_eq!(log[1].op, "act:fill");
        assert!(log[1].detail.starts_with("password_field_refused"));
        assert_eq!(log[2].op, "act:click");
        assert_eq!(log[2].detail, "click button \"Press me\" e11");
        assert_eq!(log[3].op, "act:type");
        assert_eq!(log[3].detail, "type 11 chars into textbox \"Name\" e14");
        assert!(state
            .activity(20, None, None)
            .iter()
            .all(|e| !e.detail.contains("secret") && !e.detail.contains("hunter2")));
    });
    hub.apply_config(&BrowserConfig {
        type_into_password_fields: true,
        ..window_config()
    });
    hub.run(
        &actor,
        params(BrowserOp::Fill {
            ref_: Some("e16".into()),
            selector: None,
            text: "hunter2".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        shared.lock().unwrap().ops.last().unwrap().2["allow_password"],
        true
    );
    hub.apply_config(&BrowserConfig {
        allow_act: false,
        ..window_config()
    });
    assert_eq!(
        hub.run(
            &actor,
            params(BrowserOp::Click {
                ref_: Some("e11".into()),
                selector: None
            })
        )
        .unwrap_err()
        .code,
        "act_disabled"
    );
    let _ = std::fs::remove_dir_all(&home);
}

fn step(op: BrowserOp) -> crate::api::schema::BrowserBatchStep {
    crate::api::schema::BrowserBatchStep { tab: None, op }
}

#[test]
fn batch_runs_steps_in_order_stops_on_error_and_logs_each() {
    let (hub, shared, _stream, home) = hub("batch");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let ops_before = shared.lock().unwrap().ops.len();
    let batch = hub
        .run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![
                    step(BrowserOp::Fill {
                        ref_: Some("e14".into()),
                        selector: None,
                        text: "x".into(),
                    }),
                    step(BrowserOp::Click {
                        ref_: Some("e11".into()),
                        selector: None,
                    }),
                    step(BrowserOp::Hover {
                        ref_: Some("e99".into()),
                        selector: None,
                    }),
                    step(BrowserOp::Links {
                        filter: None,
                        max: None,
                    }),
                ],
                stop_on_error: true,
                final_: Some("snapshot".into()),
                close_opened: false,
                animate: true,
            }),
        )
        .unwrap();
    assert!(
        batch
            .text
            .contains(" 1. act:fill   ok      fill 1 chars into textbox"),
        "{}",
        batch.text
    );
    assert!(batch.text.contains(" 2. act:click  ok"), "{}", batch.text);
    assert!(
        batch.text.contains(" 3. act:hover  error   stale_ref"),
        "{}",
        batch.text
    );
    assert!(
        batch.text.contains(" 4. links      skipped"),
        "{}",
        batch.text
    );
    assert!(
        batch
            .text
            .contains("[4 steps · 2 ok · 1 failed · 1 skipped]"),
        "{}",
        batch.text
    );
    assert!(
        !batch.text.contains("[ref="),
        "no final snapshot after a stop"
    );
    assert_eq!(
        shared.lock().unwrap().ops.len() - ops_before,
        3,
        "fill, click, hover reached the sidecar"
    );
    hub.with_state(|state| {
        let log = state.activity(10, None, None);
        assert_eq!(log[0].op, "batch");
        assert_eq!(log[0].detail, "4 steps · 2 ok · 1 failed");
        assert_eq!(log[1].op, "act:hover");
        assert_eq!(log[2].op, "act:click");
        assert_eq!(log[3].op, "act:fill");
    });
    let batch = hub
        .run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![
                    step(BrowserOp::Hover {
                        ref_: Some("e99".into()),
                        selector: None,
                    }),
                    step(BrowserOp::Links {
                        filter: None,
                        max: None,
                    }),
                ],
                stop_on_error: false,
                final_: Some("snapshot".into()),
                close_opened: false,
                animate: true,
            }),
        )
        .unwrap();
    assert!(batch.text.contains(" 2. links      ok"), "{}", batch.text);
    assert!(
        batch
            .text
            .contains("[3 steps · 2 ok · 1 failed · 0 skipped]"),
        "{}",
        batch.text
    );
    assert_eq!(batch.data["final"]["format"], "snapshot");
    let long: Vec<_> = (0..21)
        .map(|_| {
            step(BrowserOp::Links {
                filter: None,
                max: None,
            })
        })
        .collect();
    assert_eq!(
        hub.run(
            &actor,
            params(BrowserOp::Batch {
                ops: long,
                stop_on_error: true,
                final_: None,
                close_opened: false,
                animate: true
            })
        )
        .unwrap_err()
        .code,
        "batch_too_long"
    );
    let nested = vec![step(BrowserOp::Batch {
        ops: vec![],
        stop_on_error: true,
        final_: None,
        close_opened: false,
        animate: true,
    })];
    assert_eq!(
        hub.run(
            &actor,
            params(BrowserOp::Batch {
                ops: nested,
                stop_on_error: true,
                final_: None,
                close_opened: false,
                animate: true
            })
        )
        .unwrap_err()
        .code,
        "invalid_request"
    );
    assert_eq!(
        hub.run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![],
                stop_on_error: true,
                final_: None,
                close_opened: false,
                animate: true
            })
        )
        .unwrap_err()
        .code,
        "invalid_request"
    );
    assert_eq!(
        hub.run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![step(BrowserOp::Links {
                    filter: None,
                    max: None
                })],
                stop_on_error: true,
                final_: Some("video".into()),
                close_opened: false,
                animate: true
            })
        )
        .unwrap_err()
        .code,
        "invalid_request"
    );
    // an `open` step: the sidecar's early `opened` event and the reply make ONE record
    let before = hub.with_state(|state| state.tabs.len());
    let batch = hub
        .run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![
                    step(BrowserOp::Open {
                        url: "https://o.test/".into(),
                        focus: false,
                        wait: None,
                    }),
                    step(BrowserOp::Links {
                        filter: None,
                        max: None,
                    }),
                ],
                stop_on_error: true,
                final_: None,
                close_opened: false,
                animate: true,
            }),
        )
        .unwrap();
    assert!(batch.text.contains(" 1. open       ok"), "{}", batch.text);
    hub.with_state(|state| {
        assert_eq!(
            state.tabs.len(),
            before + 1,
            "one record for the opened tab"
        );
        let opened = state
            .cursor("w2:pD")
            .expect("the cursor moved to the opened tab")
            .clone();
        assert_eq!(
            opened.opened_by.pane_id(),
            Some("w2:pD"),
            "attributed to the agent, not the event's user"
        );
        assert_ne!(opened.target_id, "T1", "a new target, not the first open's");
    });
    let mut use_step = step(BrowserOp::Use);
    use_step.tab = Some("t1".into());
    let batch = hub
        .run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![
                    use_step,
                    step(BrowserOp::Links {
                        filter: None,
                        max: None,
                    }),
                ],
                stop_on_error: true,
                final_: None,
                close_opened: false,
                animate: true,
            }),
        )
        .unwrap();
    assert!(batch.text.contains(" 1. use        ok"), "{}", batch.text);
    let (_, target, _) = shared.lock().unwrap().ops.last().cloned().unwrap();
    assert_eq!(target.as_deref(), Some("U1"), "links ran on the user's tab");
    hub.with_state(|state| assert_eq!(state.cursor("w2:pD").unwrap().target_id, "U1"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn cursors_remember_the_pane_s_herdr_tab() {
    let (hub, _shared, _stream, home) = hub("cursor-tab");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let info = hub.get(None);
    assert_eq!(info.recent_panes[0].pane_id, "w2:pD");
    assert_eq!(info.recent_panes[0].tab_id.as_deref(), Some("w2:tD"));
    assert_eq!(info.recent_panes[0].current, "main:t2");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_snapshot_step_serves_the_next_steps_and_close_opened_tidies_up() {
    let (hub, shared, _stream, home) = hub("batch-snapshot");
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://home.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let home_key = hub.with_state(|state| state.cursor("w2:pD").unwrap().key());
    let tabs_before = hub.with_state(|state| state.open_tabs("main").count());
    let batch = hub
        .run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![
                    step(BrowserOp::Open {
                        url: "https://form.test/".into(),
                        focus: false,
                        wait: None,
                    }),
                    step(BrowserOp::Snapshot {
                        selector: None,
                        ref_: None,
                        offset: None,
                        max: Some(300),
                        interactive: true,
                    }),
                    step(BrowserOp::Fill {
                        ref_: Some("e14".into()),
                        selector: None,
                        text: "x".into(),
                    }),
                    step(BrowserOp::Click {
                        ref_: Some("e11".into()),
                        selector: None,
                    }),
                ],
                stop_on_error: true,
                final_: Some("snapshot".into()),
                close_opened: true,
                animate: true,
            }),
        )
        .unwrap();
    assert!(batch.text.contains(" 1. open       ok"), "{}", batch.text);
    assert!(
        batch
            .text
            .contains(" 2. snapshot   ok      snapshot 0–300/500"),
        "{}",
        batch.text
    );
    assert!(
        batch.text.contains("\n      abcdefghij"),
        "the snapshot's output sits under its line: {}",
        batch.text
    );
    assert!(batch.text.contains(" 3. act:fill   ok"), "{}", batch.text);
    assert!(batch.text.contains(" 4. act:click  ok"), "{}", batch.text);
    assert!(
        batch.text.contains(" 5. read       ok      snapshot"),
        "the final snapshot: {}",
        batch.text
    );
    assert!(
        batch.text.contains(" 6. close      ok"),
        "close_opened after the final step: {}",
        batch.text
    );
    assert_eq!(
        batch.data["steps"][1]["output"]
            .as_str()
            .map(|o| o.starts_with("abcdefghij")),
        Some(true)
    );
    assert_eq!(
        batch.text.matches("[chars 0–300 of 500").count(),
        1,
        "the step snapshot once, under its line: {}",
        batch.text
    );
    assert_eq!(
        batch.text.matches("[500 chars]").count(),
        1,
        "the final snapshot once, at the end: {}",
        batch.text
    );
    assert!(
        batch.data["steps"][4]["output"].is_null(),
        "the final snapshot is not duplicated into its step"
    );
    assert!(
        !batch.header.contains("closed"),
        "the header is the final step's, not the close's: {}",
        batch.header
    );
    let home_id = hub.with_state(|state| state.tabs.get(&home_key).unwrap().id());
    assert_eq!(
        batch.tab.as_deref(),
        Some(home_id.as_str()),
        "the result's tab is the one the pane is back on"
    );
    // the snapshot step ran on the opened tab, with interactive
    let snapshot_call = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .find(|(op, _, args)| {
            op == "read" && args["format"] == "snapshot" && args["interactive"] == true
        })
        .cloned();
    let (_, target, _) = snapshot_call.expect("a snapshot read");
    let fill_call = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .find(|(op, _, args)| op == "act" && args["kind"] == "fill")
        .cloned()
        .unwrap();
    assert_eq!(
        fill_call.1, target,
        "the fill ran on the same tab the snapshot was taken on"
    );
    hub.with_state(|state| {
        assert_eq!(
            state.open_tabs("main").count(),
            tabs_before,
            "the opened tab was closed again"
        );
        assert_eq!(
            state.cursor("w2:pD").unwrap().key(),
            home_key,
            "the cursor is back on the tab from before the batch"
        );
        let log = state.activity(3, None, None);
        assert_eq!(log[0].op, "batch");
        assert_eq!(log[1].op, "close");
    });
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_agent_call_carries_the_activity_directive_and_the_user_s_does_not() {
    let (hub, shared, _stream, home) = hub("activity-directive");
    let actor = pane("w2:pA");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    hub.run(
        &actor,
        params(BrowserOp::Click {
            ref_: Some("e11".into()),
            selector: None,
        }),
    )
    .unwrap();
    {
        let ops = shared.lock().unwrap().ops.clone();
        let open = ops.iter().find(|(op, _, _)| op == "open").unwrap();
        let directive = &open.2["_activity"];
        assert_eq!(directive["frame"], true, "{directive}");
        assert_eq!(directive["animate"], true);
        assert_eq!(directive["group"]["key"], "w2:pA");
        assert_eq!(directive["group"]["title"], "✻ planner");
        assert_eq!(
            directive["group"]["color"],
            crate::browser::activity::group_color("w2:pA")
        );
        assert_eq!(directive["group"]["collapse_ms"], 120_000);
        assert_eq!(directive["linger_ms"], 120_000);
        let act = ops.iter().find(|(op, _, _)| op == "act").unwrap();
        assert_eq!(
            act.2["_activity"]["frame"], true,
            "every request of the call carries it"
        );
        assert!(
            ops.iter()
                .filter(|(op, _, _)| op == "attach" || op == "hello")
                .all(|(_, _, a)| a.get("_activity").is_none()),
            "housekeeping never does"
        );
    }
    // The user's own call draws nothing.
    let user_tab = hub.with_state(|state| state.open_tabs("main").next().unwrap().id());
    let read = BrowserRunParams {
        tab: Some(user_tab),
        ..params(BrowserOp::Read {
            format: None,
            selector: None,
            ref_: None,
            offset: None,
            max: None,
            all: false,
            interactive: false,
        })
    };
    hub.run(&BrowserActor::User, read).unwrap();
    {
        let ops = shared.lock().unwrap().ops.clone();
        let read = ops.iter().rev().find(|(op, _, _)| op == "read").unwrap();
        assert!(read.2.get("_activity").is_none(), "{:?}", read.2);
    }
    // Off by config: nothing, even for a pane.
    hub.apply_config(&BrowserConfig {
        show_activity: false,
        ..window_config()
    });
    hub.run(
        &actor,
        params(BrowserOp::Click {
            ref_: Some("e11".into()),
            selector: None,
        }),
    )
    .unwrap();
    {
        let ops = shared.lock().unwrap().ops.clone();
        let act = ops.iter().rev().find(|(op, _, _)| op == "act").unwrap();
        assert!(act.2.get("_activity").is_none(), "{:?}", act.2);
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_batch_without_animate_keeps_the_frame_and_group_but_not_the_glide() {
    let (hub, shared, _stream, home) = hub("activity-batch");
    let actor = pane("w2:pB");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    hub.run(
        &actor,
        params(BrowserOp::Batch {
            ops: vec![
                step(BrowserOp::Click {
                    ref_: Some("e11".into()),
                    selector: None,
                }),
                step(BrowserOp::Fill {
                    ref_: Some("e17".into()),
                    selector: None,
                    text: "x".into(),
                }),
            ],
            stop_on_error: true,
            final_: None,
            close_opened: false,
            animate: false,
        }),
    )
    .unwrap();
    let ops = shared.lock().unwrap().ops.clone();
    let acts: Vec<&Value> = ops
        .iter()
        .filter(|(op, _, _)| op == "act")
        .map(|(_, _, a)| a)
        .collect();
    assert_eq!(acts.len(), 2);
    for act in acts {
        assert_eq!(act["_activity"]["frame"], true, "{act}");
        assert_eq!(act["_activity"]["animate"], false, "{act}");
        assert_eq!(act["_activity"]["group"]["key"], "w2:pB");
    }
    // The next single call animates again (the directive is per call).
    hub.run(
        &actor,
        params(BrowserOp::Click {
            ref_: Some("e11".into()),
            selector: None,
        }),
    )
    .unwrap();
    let ops = shared.lock().unwrap().ops.clone();
    let last = ops.iter().rev().find(|(op, _, _)| op == "act").unwrap();
    assert_eq!(last.2["_activity"]["animate"], true);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn gone_panes_release_their_groups_once() {
    let (hub, shared, _stream, home) = hub("activity-release");
    let actor = pane("w2:pC");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    hub.release_panes(vec!["w2:pC".into(), "w2:pZ".into()]);
    hub.release_panes(vec!["w2:pC".into()]);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut releases: Vec<Value> = Vec::new();
    while Instant::now() < deadline {
        releases = shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|(op, _, _)| op == "release")
            .map(|(_, _, a)| a.clone())
            .collect();
        if !releases.is_empty() {
            std::thread::sleep(Duration::from_millis(150));
            releases = shared
                .lock()
                .unwrap()
                .ops
                .iter()
                .filter(|(op, _, _)| op == "release")
                .map(|(_, _, a)| a.clone())
                .collect();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        releases.len(),
        1,
        "one release for the two panes, none for the repeat: {releases:?}"
    );
    assert_eq!(releases[0]["keys"], json!(["w2:pC", "w2:pZ"]));
    assert!(
        releases[0].get("_activity").is_none(),
        "a release carries no directive"
    );
    // The pane seen again in a call is armed again.
    hub.run(
        &actor,
        params(BrowserOp::Click {
            ref_: Some("e11".into()),
            selector: None,
        }),
    )
    .unwrap();
    hub.release_panes(vec!["w2:pC".into()]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let n = shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|(op, _, _)| op == "release")
            .count();
        if n == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|(op, _, _)| op == "release")
            .count(),
        2
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn attach_records_the_companion_state_for_status() {
    let (hub, shared, _stream, home) = hub("activity-companion");
    shared.lock().unwrap().companion = Some(
        json!({ "state": "missing", "detail": "no companion service worker on the DevTools port" }),
    );
    let actor = pane("w2:pD");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let status = hub.status();
    let main = status
        .get
        .profiles
        .iter()
        .find(|p| p.name == "main")
        .unwrap();
    assert_eq!(
        main.companion.as_deref(),
        Some("missing (no companion service worker on the DevTools port)")
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_failed_release_is_retried_later_not_on_every_poll() {
    let (hub, shared, _stream, home) = hub("activity-release-retry");
    let actor = pane("w2:pE");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    hub.release_panes(vec!["w2:pE".into(), "w2:pfail".into()]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && hub.release_attempts_for_test("w2:pfail").is_none() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        hub.release_attempts_for_test("w2:pfail"),
        Some(1),
        "the failed key is armed for a retry"
    );
    assert_eq!(
        hub.release_attempts_for_test("w2:pE"),
        None,
        "the released key is done"
    );
    // Polls within the backoff send nothing more for the failed key.
    hub.release_panes(vec!["w2:pfail".into(), "w2:pE".into()]);
    std::thread::sleep(Duration::from_millis(200));
    let releases: Vec<Value> = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .filter(|(op, _, _)| op == "release")
        .map(|(_, _, a)| a.clone())
        .collect();
    assert_eq!(releases.len(), 1, "{releases:?}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_open_whose_page_failed_still_adopts_the_tab_and_close_opened_covers_it() {
    let (hub, shared, _stream, home) = hub("open-failed-adopt");
    shared.lock().unwrap().errors.push(("open".into(), "navigation_failed".into(), "net::ERR_NAME_NOT_RESOLVED (the new tab is your current tab; browser read shows what loaded)".into()));
    let actor = pane("w2:pF");
    let err = hub
        .run(
            &actor,
            params(BrowserOp::Open {
                url: "https://nowhere.test/".into(),
                focus: false,
                wait: None,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "navigation_failed");
    assert_eq!(err.target.as_deref(), Some("T1"));
    hub.with_state(|state| {
        let record = state
            .tabs
            .get(&TabKey::new("main", "T1"))
            .expect("the tab is in the ledger");
        assert!(record.is_open());
        assert_eq!(record.url, "https://nowhere.test/");
        assert_eq!(
            record.opened_by.pane_id(),
            Some("w2:pF"),
            "attributed to the caller, not herdr"
        );
        assert_eq!(
            state.cursor("w2:pF").unwrap().key(),
            record.key(),
            "and it is the pane's current tab"
        );
    });
    // In a batch the failed open's tab is still closed by close_opened.
    let batch = hub
        .run(
            &actor,
            params(BrowserOp::Batch {
                ops: vec![step(BrowserOp::Open {
                    url: "https://nowhere.test/2".into(),
                    focus: false,
                    wait: None,
                })],
                stop_on_error: false,
                final_: None,
                close_opened: true,
                animate: true,
            }),
        )
        .unwrap();
    assert!(
        batch
            .text
            .contains(" 1. open       error   navigation_failed"),
        "{}",
        batch.text
    );
    assert!(
        batch.text.contains(" 2. close      ok      t3"),
        "{}",
        batch.text
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn eval_is_guarded_by_the_password_rule_and_logs_the_code_not_the_result() {
    let (hub, shared, _stream, home) = hub("eval-guard");
    let actor = pane("w2:pG");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    let err = hub
        .run(
            &actor,
            params(BrowserOp::Eval {
                expr: "document.querySelector('#pw2').value='x'".into(),
                max: None,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "password_field_refused");
    let ok = hub
        .run(
            &actor,
            params(BrowserOp::Eval {
                expr: "document.title\u{7}".into(),
                max: None,
            }),
        )
        .unwrap();
    assert!(
        ok.text.contains("\"a\": 1") || ok.text.contains("{\"a\":1}"),
        "{}",
        ok.text
    );
    {
        let ops = shared.lock().unwrap().ops.clone();
        let evals: Vec<&Value> = ops
            .iter()
            .filter(|(op, _, _)| op == "eval")
            .map(|(_, _, a)| a)
            .collect();
        assert_eq!(evals.len(), 2);
        assert_eq!(
            evals[0]["guard_passwords"], true,
            "the default config guards"
        );
    }
    hub.with_state(|state| {
        let log = state.activity(3, None, None);
        assert_eq!(log[0].op, "eval");
        assert_eq!(
            log[0].detail, "eval: document.title",
            "sanitized code, no result"
        );
        assert!(!log[0].detail.contains("\"a\""));
        assert_eq!(log[1].op, "eval");
        assert!(
            log[1].detail.contains("password_field_refused"),
            "{}",
            log[1].detail
        );
        assert!(
            log[1]
                .detail
                .contains("eval: document.querySelector('#pw2').value='x'"),
            "a refused eval still shows its code: {}",
            log[1].detail
        );
    });
    // Long code is clipped in the ledger; a long string literal is masked first.
    let long = format!("document.title + '{}'", "y".repeat(400));
    hub.run(
        &actor,
        params(BrowserOp::Eval {
            expr: long,
            max: None,
        }),
    )
    .unwrap();
    hub.with_state(|state| {
        let detail = &state.activity(1, None, None)[0].detail;
        assert_eq!(detail, "eval: document.title + \"‹400 chars›\"", "{detail}");
        assert!(
            !detail.contains("yyyy"),
            "the literal never reaches the ledger"
        );
        assert!(detail.chars().count() <= 220, "{}", detail.chars().count());
    });
    // Passwords allowed by config: no guard.
    hub.apply_config(&BrowserConfig {
        type_into_password_fields: true,
        ..window_config()
    });
    hub.run(
        &actor,
        params(BrowserOp::Eval {
            expr: "document.querySelector('#pw2').value='x'".into(),
            max: None,
        }),
    )
    .unwrap();
    let last = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .rev()
        .find(|(op, _, _)| op == "eval")
        .map(|(_, _, a)| a.clone())
        .unwrap();
    assert_eq!(last["guard_passwords"], false);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_new_tab_page_snapshot_is_pushed_after_changes_at_most_once_a_second() {
    let (hub, shared, _stream, home) = hub("ntp-push");
    let actor = pane("w2:pH");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/page".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    for _ in 0..4 {
        hub.run(
            &actor,
            params(BrowserOp::Click {
                ref_: Some("e11".into()),
                selector: None,
            }),
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(1400));
    let pushes: Vec<Value> = shared.lock().unwrap().ntp.clone();
    assert!(!pushes.is_empty(), "a push went out");
    assert!(
        pushes.len() <= 3,
        "five changes in a burst are at most a few pushes: {}",
        pushes.len()
    );
    let last = &pushes[pushes.len() - 1]["snapshot"];
    assert_eq!(last["version"], crate::browser::ntp::NTP_SNAPSHOT_VERSION);
    assert_eq!(last["show_activity"], true);
    let agents = last["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{last}");
    assert_eq!(agents[0]["label"], "planner");
    assert_eq!(agents[0]["symbol"], "✻");
    let tabs = agents[0]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1, "{last}");
    assert_eq!(tabs[0]["current"], true);
    assert!(tabs[0]["short"]
        .as_str()
        .is_some_and(|s| s.starts_with('t')));
    assert_eq!(agents[0]["last_op"], "act:click");
    assert_eq!(agents[0]["active"], true);
    assert!(
        pushes.iter().all(|p| p.get("_activity").is_none()),
        "a push carries no directive"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_dashboard_pin_travels_with_attach_and_follows_the_config() {
    let (hub, shared, _stream, home) = hub("dashboard-pin");
    let actor = pane("w2:pI");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        }),
    )
    .unwrap();
    {
        let ops = shared.lock().unwrap().ops.clone();
        let attach = ops.iter().find(|(op, _, _)| op == "attach").unwrap();
        assert_eq!(attach.2["pin_dashboard"], true, "the default pins");
    }
    hub.apply_config(&BrowserConfig {
        pin_dashboard: false,
        ..window_config()
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut pushes: Vec<Value> = Vec::new();
    while Instant::now() < deadline {
        pushes = shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|(op, _, _)| op == "dashboard")
            .map(|(_, _, a)| a.clone())
            .collect();
        if !pushes.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert_eq!(pushes[0]["pin"], false);
    // the same value again: no push; show_activity off: unpinned
    hub.apply_config(&BrowserConfig {
        pin_dashboard: false,
        ..window_config()
    });
    hub.apply_config(&BrowserConfig {
        show_activity: false,
        ..window_config()
    });
    std::thread::sleep(Duration::from_millis(300));
    let pushes: Vec<Value> = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .filter(|(op, _, _)| op == "dashboard")
        .map(|(_, _, a)| a.clone())
        .collect();
    assert_eq!(
        pushes.len(),
        1,
        "unchanged effective value, no push: {pushes:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn tabs_are_selected_quietly_and_only_focus_raises_the_window() {
    let (hub, shared, _stream, home) = hub("quiet-select");
    let actor = pane("w2:pA");
    hub.run(
        &actor,
        params(BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: true,
            wait: None,
        }),
    )
    .unwrap();
    hub.run(
        &actor,
        params(BrowserOp::Screenshot {
            full: false,
            ref_: None,
            selector: None,
            format: None,
            out: None,
            front: true,
        }),
    )
    .unwrap();
    hub.run(&actor, params(BrowserOp::Focus)).unwrap();
    let ops = shared.lock().unwrap().ops.clone();
    let open = ops.iter().find(|(op, _, _)| op == "open").unwrap();
    assert_eq!(
        open.2["background"], true,
        "open --focus never asks for a foreground target"
    );
    let names: Vec<&str> = ops.iter().map(|(op, _, _)| op.as_str()).collect();
    // open --focus and screenshot --front select the tab quietly; browser focus raises
    assert_eq!(
        names.iter().filter(|n| **n == "select").count(),
        2,
        "{names:?}"
    );
    let shot = names.iter().position(|n| *n == "screenshot").unwrap();
    assert_eq!(
        names[shot - 1],
        "select",
        "selected before the shot: {names:?}"
    );
    let focus = names.iter().position(|n| *n == "focus").unwrap();
    assert_eq!(
        names.iter().filter(|n| **n == "focus").count(),
        1,
        "only browser focus: {names:?}"
    );
    assert!(focus > shot, "{names:?}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn without_a_companion_open_focus_still_opens_and_screenshot_front_fails_quietly() {
    let (hub, shared, _stream, home) = hub("no-quiet-select");
    shared.lock().unwrap().errors.push((
        "select".into(),
        "tab_not_selected".into(),
        "the tab could not be selected quietly: the companion extension is not ready or does not know this tab; the window was not raised — `browser focus` brings it up for the user".into(),
    ));
    let actor = pane("w2:pA");
    let opened = hub
        .run(
            &actor,
            params(BrowserOp::Open {
                url: "https://a.test/".into(),
                focus: true,
                wait: None,
            }),
        )
        .unwrap();
    assert!(
        opened.text.contains("(the tab was not selected — the companion extension is not ready or does not know this tab; the window was not raised)"),
        "{}",
        opened.text
    );
    let record = hub.with_state(|state| {
        state
            .open_tabs("main")
            .find(|r| r.target_id.starts_with('T'))
            .unwrap()
            .clone()
    });
    assert!(
        !record.selected,
        "not selected, and the open still succeeded"
    );
    let err = hub
        .run(
            &actor,
            params(BrowserOp::Screenshot {
                full: false,
                ref_: None,
                selector: None,
                format: None,
                out: None,
                front: true,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "tab_not_selected", "{}", err.message);
    assert!(
        err.message.contains("the window was not raised"),
        "{}",
        err.message
    );
    let names: Vec<String> = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .map(|(op, _, _)| op.clone())
        .collect();
    assert!(
        !names.iter().any(|n| n == "focus"),
        "nothing raised the window: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "screenshot"),
        "no shot after a refused select: {names:?}"
    );
    // other causes are named on the card
    shared.lock().unwrap().errors.clear();
    shared.lock().unwrap().errors.push((
        "select".into(),
        "browser_timeout".into(),
        "select exceeded 5000 ms".into(),
    ));
    let opened = hub
        .run(
            &actor,
            params(BrowserOp::Open {
                url: "https://b.test/".into(),
                focus: true,
                wait: None,
            }),
        )
        .unwrap();
    assert!(
        opened.text.contains("(the tab was not selected — browser_timeout: select exceeded 5000 ms; the window was not raised)"),
        "{}",
        opened.text
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_blocked_attach_names_what_the_sidecar_saw_and_never_advises_a_stop() {
    let (hub, shared, _stream, home) = hub("attach-blocked");
    shared.lock().unwrap().errors.push((
        "attach".into(),
        "attach_timeout".into(),
        "the browser did not accept the CDP connection within 15000 ms: these tabs are not responding (a dialog may be open in one): \"Checkout — shop.test\"; answer or close it in the herdr+ Browser window, then retry".into(),
    ));
    let actor = pane("w2:pA");
    let err = hub
        .run(
            &actor,
            params(BrowserOp::Open {
                url: "https://a.test/".into(),
                focus: false,
                wait: None,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "attach_blocked", "{}", err.message);
    assert!(
        err.message.starts_with("the browser did not accept the attach within 15 s (twice): these tabs are not responding (a dialog may be open in one): \"Checkout — shop.test\"; answer or close it in the herdr+ Browser window, then retry. Your tabs are safe"),
        "{}",
        err.message
    );
    assert!(
        err.message.contains("do not stop it for this")
            && !err.message.contains("`herdr browser stop`"),
        "never a stop: {}",
        err.message
    );
    let names: Vec<String> = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .map(|(op, _, _)| op.clone())
        .collect();
    assert_eq!(
        names.iter().filter(|n| *n == "attach").count(),
        2,
        "one retry, then the verdict: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "close_browser"),
        "no stop, no relaunch: {names:?}"
    );
    assert!(
        hub.with_state(|state| matches!(
            state.profile("main"),
            crate::browser::state::ProfileStatus::Running { .. }
        )),
        "the profile stays running"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_hub_side_attach_timeout_gets_the_fixed_clause() {
    let (hub, shared, _stream, home) = hub("attach-hub-timeout");
    shared.lock().unwrap().errors.push((
        "attach".into(),
        "browser_timeout".into(),
        "attach exceeded 15000 ms".into(),
    ));
    let actor = pane("w2:pA");
    let err = hub
        .run(
            &actor,
            params(BrowserOp::Open {
                url: "https://a.test/".into(),
                focus: false,
                wait: None,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "attach_blocked", "{}", err.message);
    assert!(
        err.message.contains("the browser did not answer in time: a dialog, a frozen page, or many tabs still loading; look at the herdr+ Browser window, then retry. Your tabs are safe"),
        "{}",
        err.message
    );
    assert!(
        !err.message.contains("exceeded"),
        "the raw timeout text stays out: {}",
        err.message
    );
    let _ = std::fs::remove_dir_all(&home);
}

// ----- Fork (companion browser) ---------------------------------------------

fn register_params(instance: &str) -> crate::api::schema::BrowserHostRegisterParams {
    crate::api::schema::BrowserHostRegisterParams {
        kind: crate::api::schema::BrowserHostKind::Companion,
        instance: instance.into(),
        pid: Some(4242),
        cdp: crate::api::schema::BrowserHostCdp {
            host: "127.0.0.1".into(),
            port: 49731,
        },
        nonce: format!("nonce-{instance}"),
        version: "0.1.0".into(),
        protocol: 1,
        capabilities: vec!["views".into()],
        launch_token: None,
        managed: false,
    }
}

fn open_op(url: &str) -> BrowserOp {
    BrowserOp::Open {
        url: url.into(),
        focus: false,
        wait: None,
    }
}

#[test]
fn fork_smoke_a_registered_companion_takes_new_tabs_and_the_window_takes_them_back_when_its_lease_expires(
) {
    let (hub, shared, _stream, _home) = hub("companion-routing");
    let lease = hub
        .host_register(&register_params("a"))
        .expect("registered");
    assert_eq!(lease.profile, "companion");
    assert_eq!(lease.state, "attached");
    assert!(lease.server_features.iter().any(|f| f == "show"));
    assert!(hub.companion_live());
    let actor = pane("w2:pD");
    let opened = hub
        .run(&actor, params(open_op("https://a.test/")))
        .expect("open");
    assert!(opened
        .tab
        .as_deref()
        .is_some_and(|tab| tab.starts_with("companion:")));
    assert_eq!(opened.data["host"], "companion");
    {
        let state = shared.lock().unwrap();
        let open = state
            .ops
            .iter()
            .rev()
            .find(|(op, _, _)| op == "open")
            .unwrap();
        assert_eq!(
            open.2["tab_id"], "w2:tD",
            "the view goes to the caller's group"
        );
        assert_eq!(open.2["opened_by"], "agent:w2:pD");
    }
    // The heartbeat keeps it; an expired lease goes on the supervisor's pass.
    assert!(hub.host_heartbeat(&lease.lease_id).is_ok());
    hub.expire_host_lease_for_test();
    hub.supervise();
    assert!(!hub.companion_live());
    assert!(matches!(
        hub.host_heartbeat(&lease.lease_id),
        Err(err) if err.code == "lease_unknown"
    ));
    hub.with_state(|state| {
        assert!(matches!(
            state.profile("companion"),
            super::state::ProfileStatus::Crashed { .. }
        ));
        assert!(state
            .tabs
            .values()
            .filter(|record| record.profile == "companion")
            .all(|record| !record.is_open()));
    });
    // Silent fallback: the next open goes to the window, `host` says so.
    let back = hub
        .run(&actor, params(open_op("https://b.test/")))
        .expect("open");
    assert!(back
        .tab
        .as_deref()
        .is_some_and(|tab| tab.starts_with("main:")));
    assert_eq!(back.data["host"], "window");
}

#[test]
fn fork_smoke_a_second_companion_is_refused_while_the_first_lease_lives() {
    let (hub, _shared, _stream, _home) = hub("companion-in-use");
    let first = hub
        .host_register(&register_params("a"))
        .expect("registered");
    let err = hub.host_register(&register_params("b")).unwrap_err();
    assert_eq!(err.code, "host_in_use");
    assert!(err.message.contains("instance a"), "{}", err.message);
    // The same instance re-registering (a reconnect) replaces its lease.
    let again = hub
        .host_register(&register_params("a"))
        .expect("re-registered");
    assert_ne!(again.lease_id, first.lease_id);
    assert!(!hub.lease_matches(&first.lease_id));
    // Unregister frees the slot for another instance.
    hub.host_unregister(&again.lease_id).expect("unregistered");
    assert!(hub.host_register(&register_params("b")).is_ok());
    // Bad registrations.
    let mut remote = register_params("c");
    remote.cdp.host = "10.0.0.2".into();
    let (hub2, _s2, _st2, _h2) = self::hub("companion-bad");
    assert_eq!(
        hub2.host_register(&remote).unwrap_err().code,
        "invalid_request"
    );
    let mut old = register_params("c");
    old.protocol = 2;
    assert_eq!(
        hub2.host_register(&old).unwrap_err().code,
        "host_protocol_unsupported"
    );
    let config = BrowserConfig {
        host: "window".into(),
        ..window_config()
    };
    hub2.apply_config(&config);
    assert_eq!(
        hub2.host_register(&register_params("c")).unwrap_err().code,
        "host_disabled"
    );
}

#[test]
fn a_pending_companion_registration_that_died_mid_attach_times_out() {
    let (hub, _shared, _stream, _home) = hub("companion-pending");
    // A registration still within its attach window blocks another instance.
    hub.install_pending_registration_for_test("dead", std::time::Duration::from_secs(1));
    assert_eq!(
        hub.host_register(&register_params("b")).unwrap_err().code,
        "host_in_use"
    );
    // One whose register call never came back past PENDING_TIMEOUT no longer
    // does, and the supervisor pass drops it.
    hub.install_pending_registration_for_test("dead", std::time::Duration::from_secs(3600));
    hub.supervise();
    assert!(!hub.companion_live());
    assert!(hub.host_register(&register_params("b")).is_ok());
}

#[test]
fn a_companion_attach_skips_the_extension_sweep_and_checks_the_nonce() {
    let (hub, shared, _stream, _home) = hub("companion-attach");
    hub.host_register(&register_params("a"))
        .expect("registered");
    let state = shared.lock().unwrap();
    let attach = state
        .ops
        .iter()
        .find(|(op, _, _)| op == "attach")
        .expect("attached");
    assert_eq!(attach.2["kind"], "companion");
    assert_eq!(attach.2["nonce"], "nonce-a");
    assert_eq!(attach.2["port"], 49731);
    assert!(
        attach.2.get("pin_dashboard").is_none(),
        "no dashboard for a companion"
    );
    drop(state);
    // A failed nonce check drops the registration.
    let (hub2, shared2, _stream2, _home2) = self::hub("companion-nonce");
    shared2.lock().unwrap().errors.push((
        "attach".into(),
        "host_verify_failed".into(),
        "nonce mismatch".into(),
    ));
    let err = hub2.host_register(&register_params("x")).unwrap_err();
    assert_eq!(err.code, "host_verify_failed");
    assert!(!hub2.companion_live());
    assert!(
        hub2.host_register(&register_params("y")).is_err(),
        "still failing, but not host_in_use"
    );
    // The reserved profile is never created, deleted or launched.
    assert_eq!(
        hub.profile_create("companion", false).unwrap_err().code,
        "invalid_profile"
    );
}

#[test]
fn an_open_without_profile_follows_host_but_other_ops_follow_the_cursor() {
    let (hub, _shared, _stream, _home) = hub("companion-cursor");
    let actor = pane("w2:pD");
    // Before the companion: the window, and the cursor lives there.
    let first = hub
        .run(&actor, params(open_op("https://a.test/")))
        .expect("open");
    assert!(first.tab.as_deref().unwrap().starts_with("main:"));
    hub.host_register(&register_params("a"))
        .expect("registered");
    // A read follows the pane's cursor (still the window tab).
    let read = hub
        .run(
            &actor,
            params(BrowserOp::Read {
                format: None,
                selector: None,
                ref_: None,
                offset: None,
                max: None,
                all: false,
                interactive: false,
            }),
        )
        .expect("read");
    assert_eq!(read.tab, first.tab);
    // An open goes to the companion; the cursor moves with it.
    let second = hub
        .run(&actor, params(open_op("https://b.test/")))
        .expect("open");
    assert!(second.tab.as_deref().unwrap().starts_with("companion:"));
    // `[browser] host = "window"` keeps opens in the window.
    let config = BrowserConfig {
        host: "window".into(),
        ..window_config()
    };
    hub.apply_config(&config);
    let third = hub
        .run(&actor, params(open_op("https://c.test/")))
        .expect("open");
    assert!(third.tab.as_deref().unwrap().starts_with("main:"));
    // `host = "companion"` without a companion: host_unavailable.
    let (hub2, _s2, _st2, _h2) = self::hub("companion-only");
    let config = BrowserConfig {
        host: "companion".into(),
        ..window_config()
    };
    hub2.apply_config(&config);
    let err = hub2
        .run(&actor, params(open_op("https://d.test/")))
        .unwrap_err();
    assert_eq!(err.code, "host_unavailable");
}

#[test]
fn page_errors_are_batched_deduped_and_capped_per_tab() {
    let (hub, shared, _stream, _home) = hub("page-errors");
    let actor = pane("w2:pD");
    let opened = hub
        .run(&actor, params(open_op("http://localhost:5173/")))
        .expect("open");
    let target = opened.data["target_id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| "T1".into());
    let event = |kind: &str, text: &str| {
        json!({ "event": "tab", "profile": "main", "target": target, "kind": kind, "text": text })
            .to_string()
    };
    {
        let mut state = shared.lock().unwrap();
        for n in 0..12 {
            state.events.push(event("page_error", &format!("boom {n}")));
        }
        // A repeat of the first: deduped.
        state.events.push(event("page_error", "boom 0"));
    }
    // Any op makes the fake host send the queued events first (a navigate
    // keeps the tab on the local page that notices need).
    hub.run(
        &actor,
        params(BrowserOp::Navigate {
            url: "http://localhost:5173/".into(),
            wait: None,
        }),
    )
    .expect("navigate");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !hub.page_errors_pending() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        hub.take_page_errors().is_empty(),
        "the window is still open"
    );
    let later = Instant::now() + super::page_errors::BATCH_WINDOW;
    let batches = hub.take_page_errors_at(later);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].pane, "w2:pD");
    assert_eq!(
        batches[0].errors.len(),
        super::page_errors::BATCH_MAX_ERRORS
    );
    assert_eq!(
        batches[0].more, 2,
        "12 errors, 10 shown, the repeat dropped"
    );
    assert!(batches[0].message_text().contains("untrusted"));
    // Per tab: at most TAB_PUSHES batches in the window.
    let mut pushed = 1;
    for round in 0..8 {
        let mut state = shared.lock().unwrap();
        state
            .events
            .push(event("console_error", &format!("round {round}")));
        drop(state);
        hub.run(
            &actor,
            params(BrowserOp::Navigate {
                url: "http://localhost:5173/".into(),
                wait: None,
            }),
        )
        .expect("navigate");
        std::thread::sleep(Duration::from_millis(30));
        pushed += hub
            .take_page_errors_at(later + super::page_errors::BATCH_WINDOW * (round + 2))
            .len();
    }
    assert_eq!(pushed, super::page_errors::TAB_PUSHES);
    // `[browser] page_errors = "off"`: nothing queued.
    let config = BrowserConfig {
        page_errors: "off".into(),
        ..window_config()
    };
    hub.apply_config(&config);
    shared
        .lock()
        .unwrap()
        .events
        .push(event("page_error", "quiet please"));
    hub.run(
        &actor,
        params(BrowserOp::Navigate {
            url: "http://localhost:5173/".into(),
            wait: None,
        }),
    )
    .expect("navigate");
    std::thread::sleep(Duration::from_millis(50));
    assert!(!hub.page_errors_pending());
}

/// Runs any op so the fake host sends its queued events, then waits a
/// moment for the reader thread to apply them.
fn flush_host_events(hub: &BrowserHub, actor: &BrowserActor) {
    hub.run(
        actor,
        params(BrowserOp::Links {
            filter: None,
            max: None,
        }),
    )
    .expect("links");
    std::thread::sleep(Duration::from_millis(80));
}

#[test]
fn fork_smoke_page_errors_notify_only_for_pages_the_agent_builds() {
    let (hub, shared, _stream, home) = hub("page-error-scope");
    let actor = pane("w2:pD");
    let target_of = |result: &crate::api::schema::BrowserRunResult| {
        hub.get(None)
            .tabs
            .iter()
            .find(|tab| Some(tab.id.as_str()) == result.tab.as_deref())
            .map(|tab| tab.target_id.clone())
            .expect("the tab in the ledger")
    };
    let error = |target: &str, kind: &str, text: &str| {
        json!({ "event": "tab", "profile": "main", "target": target, "kind": kind, "text": text })
            .to_string()
    };
    let later = || Instant::now() + super::page_errors::BATCH_WINDOW * 2;
    // Another site: no notice, but the error is still on the tab.
    let site = target_of(
        &hub.run(&actor, params(open_op("https://apps.apple.com/us/app")))
            .expect("open"),
    );
    shared.lock().unwrap().events.extend([
        error(&site, "console_error", "CORS policy: blocked"),
        error(&site, "page_error", "TypeError: x is undefined"),
        error(
            &site,
            "load_failed",
            "net::ERR_FAILED https://apps.apple.com/us/app",
        ),
    ]);
    flush_host_events(&hub, &actor);
    assert!(
        !hub.page_errors_pending(),
        "a third-party site's errors are not sent"
    );
    assert!(hub.take_page_errors_at(later()).is_empty());
    let info = hub.get(None);
    let tab = info
        .tabs
        .iter()
        .find(|tab| tab.target_id == site)
        .expect("the site's tab");
    assert_eq!(tab.console_errors, 2, "still counted for `browser console`");
    // A loopback dev server and a local file: one notice each.
    for url in ["http://127.0.0.1:8080/app", "file:///tmp/report.html"] {
        let target = target_of(&hub.run(&actor, params(open_op(url))).expect("open"));
        shared
            .lock()
            .unwrap()
            .events
            .push(error(&target, "page_error", &format!("boom on {url}")));
        flush_host_events(&hub, &actor);
        let batches = hub.take_page_errors_at(later());
        assert_eq!(batches.len(), 1, "{url}");
        assert_eq!(batches[0].pane, "w2:pD");
    }
    // A failed load of a dev server that is down, now on Chromium's error
    // page: the request's URL in the text counts.
    let down = target_of(
        &hub.run(&actor, params(open_op("http://localhost:3000/")))
            .expect("open"),
    );
    shared.lock().unwrap().events.extend([
        json!({ "event": "tab", "profile": "main", "target": down, "kind": "navigated", "url": "chrome-error://chromewebdata/", "title": "", "initiator": "command" }).to_string(),
        error(&down, "load_failed", "net::ERR_CONNECTION_REFUSED http://localhost:3000/"),
    ]);
    flush_host_events(&hub, &actor);
    assert_eq!(hub.take_page_errors_at(later()).len(), 1);
    // A shown HTML view notifies whatever its URL became.
    let file = home.join("view.html");
    std::fs::write(&file, "<p>v</p>").unwrap();
    let shown = hub
        .run(
            &actor,
            params(BrowserOp::Show {
                file: file.display().to_string(),
                allow_network: true,
                page_api: false,
                watch: false,
                focus: false,
                title: None,
            }),
        )
        .expect("show");
    let profile = shown
        .tab
        .as_deref()
        .and_then(|tab| tab.split_once(':'))
        .map(|(profile, _)| profile.to_string())
        .expect("tab id");
    let view = target_of(&shown);
    shared.lock().unwrap().events.extend([
        json!({ "event": "tab", "profile": profile, "target": view, "kind": "navigated", "url": "https://cdn.example/after-redirect", "title": "", "initiator": "other" }).to_string(),
        json!({ "event": "tab", "profile": profile, "target": view, "kind": "page_error", "text": "view boom" }).to_string(),
    ]);
    flush_host_events(&hub, &actor);
    let batches = hub.take_page_errors_at(later());
    assert_eq!(batches.len(), 1, "an HTML view always notifies");
    assert!(batches[0].message_text().contains("view boom"));
}

#[test]
fn fork_smoke_a_failed_external_load_from_a_local_tab_sends_no_notice() {
    let (hub, shared, _stream, _home) = hub("page-error-load-failed");
    let actor = pane("w2:pD");
    let opened = hub
        .run(&actor, params(open_op("http://localhost:5173/")))
        .expect("open");
    let target = hub
        .get(None)
        .tabs
        .iter()
        .find(|tab| Some(tab.id.as_str()) == opened.tab.as_deref())
        .map(|tab| tab.target_id.clone())
        .expect("the tab in the ledger");
    // The sidecar reports the failed request before the tab navigates: the
    // ledger still says localhost, the request went elsewhere.
    shared.lock().unwrap().events.extend([
        json!({ "event": "tab", "profile": "main", "target": target, "kind": "load_failed", "text": "net::ERR_NAME_NOT_RESOLVED https://cdn.external.test/" }).to_string(),
        json!({ "event": "tab", "profile": "main", "target": target, "kind": "navigated", "url": "chrome-error://chromewebdata/", "title": "", "initiator": "other" }).to_string(),
    ]);
    flush_host_events(&hub, &actor);
    assert!(
        hub.take_page_errors_at(Instant::now() + super::page_errors::BATCH_WINDOW * 2)
            .is_empty(),
        "the request URL decides, not the page the tab was on"
    );
}

#[test]
fn a_show_outside_the_companion_opens_in_the_window_with_network_routed_off() {
    let (hub, shared, _stream, home) = hub("show-window");
    let actor = pane("w2:pD");
    let file = home.join("report.html");
    std::fs::write(&file, "<h1>hi</h1>").unwrap();
    let show = |file: &str| {
        params(BrowserOp::Show {
            file: file.into(),
            allow_network: false,
            page_api: false,
            watch: true,
            focus: false,
            title: None,
        })
    };
    let shown = hub
        .run(&actor, show(&file.display().to_string()))
        .expect("show");
    assert!(shown.tab.as_deref().unwrap().starts_with("main:"));
    assert_eq!(shown.data["host"], "window");
    assert_eq!(shown.data["kind"], "html_view");
    {
        let state = shared.lock().unwrap();
        let op = state
            .ops
            .iter()
            .rev()
            .find(|(op, _, _)| op == "show")
            .unwrap();
        assert_eq!(op.2["allow_network"], false, "network routed off");
        assert_eq!(op.2["watch"], true);
        assert_eq!(
            op.2["root"],
            std::fs::canonicalize(&home).unwrap().display().to_string()
        );
    }
    // The cursor moved to the view.
    let read = hub
        .run(
            &actor,
            params(BrowserOp::Read {
                format: None,
                selector: None,
                ref_: None,
                offset: None,
                max: None,
                all: false,
                interactive: false,
            }),
        )
        .expect("read");
    assert_eq!(read.tab, shown.tab);
    // Refusals.
    let txt = home.join("notes.txt");
    std::fs::write(&txt, "x").unwrap();
    assert_eq!(
        hub.run(&actor, show(&txt.display().to_string()))
            .unwrap_err()
            .code,
        "not_html"
    );
    assert_eq!(
        hub.run(&actor, show(&home.join("gone.html").display().to_string()))
            .unwrap_err()
            .code,
        "file_not_found"
    );
    assert_eq!(
        hub.run(&actor, show("relative.html")).unwrap_err().code,
        "invalid_request"
    );
    // With a companion the view goes there, in the caller's group.
    hub.host_register(&register_params("a"))
        .expect("registered");
    let in_companion = hub
        .run(&actor, show(&file.display().to_string()))
        .expect("show");
    assert_eq!(in_companion.data["host"], "companion");
    assert_eq!(
        in_companion.data["view_id"]
            .as_str()
            .map(|v| v.starts_with('v')),
        Some(true)
    );
    let state = shared.lock().unwrap();
    let op = state
        .ops
        .iter()
        .rev()
        .find(|(op, _, _)| op == "show")
        .unwrap();
    assert_eq!(op.1, None);
    assert_eq!(op.2["tab_id"], "w2:tD");
}

#[cfg(unix)]
#[test]
fn a_symlinked_show_file_gets_the_directory_it_really_lives_in_as_root() {
    let (hub, shared, _stream, home) = hub("show-symlink");
    let real = home.join("real");
    let links = home.join("links");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(&links).unwrap();
    std::fs::write(real.join("report.html"), "<h1>hi</h1>").unwrap();
    let link = links.join("report.html");
    std::os::unix::fs::symlink(real.join("report.html"), &link).unwrap();
    hub.run(
        &pane("w2:pD"),
        params(BrowserOp::Show {
            file: link.display().to_string(),
            allow_network: false,
            page_api: false,
            watch: true,
            focus: false,
            title: None,
        }),
    )
    .expect("show");
    let state = shared.lock().unwrap();
    let op = state
        .ops
        .iter()
        .rev()
        .find(|(op, _, _)| op == "show")
        .unwrap();
    assert_eq!(
        op.2["root"],
        std::fs::canonicalize(&real).unwrap().display().to_string()
    );
    assert_eq!(
        op.2["file"],
        link.display().to_string(),
        "the file as given"
    );
}

#[test]
fn show_takes_a_file_url_and_decodes_its_path() {
    let (hub, shared, _stream, home) = hub("show-file-url");
    let file = home.join("my report.html");
    std::fs::write(&file, "<h1>hi</h1>").unwrap();
    let path = file.display().to_string();
    let show = |file: String| {
        hub.run(
            &pane("w2:pD"),
            params(BrowserOp::Show {
                file,
                allow_network: false,
                page_api: false,
                watch: false,
                focus: false,
                title: None,
            }),
        )
    };
    for url in [
        format!("file://{}", path.replace(' ', "%20")),
        format!("FILE://localhost{}#top", path.replace(' ', "%20")),
    ] {
        show(url.clone()).expect(&url);
        let state = shared.lock().unwrap();
        let op = state
            .ops
            .iter()
            .rev()
            .find(|(op, _, _)| op == "show")
            .unwrap();
        assert_eq!(op.2["file"], path, "{url}");
    }
    let err = show(format!("file://elsewhere{path}")).unwrap_err();
    assert_eq!(
        err.code, "invalid_request",
        "another host is not a local file"
    );
    let err = show(format!("file://{}.htm", path)).unwrap_err();
    assert_eq!(err.code, "file_not_found");
}

#[test]
fn a_companion_that_goes_takes_its_html_views_with_it() {
    let (hub, _shared, _stream, home) = hub("companion-html-views");
    let lease = hub
        .host_register(&register_params("a"))
        .expect("registered");
    let file = home.join("v.html");
    std::fs::write(&file, "<p>v</p>").unwrap();
    let shown = hub
        .run(
            &pane("w2:pD"),
            params(BrowserOp::Show {
                file: file.display().to_string(),
                allow_network: false,
                page_api: false,
                watch: true,
                focus: false,
                title: None,
            }),
        )
        .expect("show");
    assert_eq!(shown.data["host"], "companion");
    assert_eq!(hub.html_views_for_test("companion"), 1);
    hub.host_unregister(&lease.lease_id).expect("unregistered");
    assert_eq!(hub.html_views_for_test("companion"), 0);
}

/// Fork (page API): `show --page-api` reaches the companion as `page_api`,
/// never with network, never while the kill switch is off; the shown page
/// counts as driven by the agent for the grace.
#[test]
fn a_page_api_show_reaches_the_companion_only_without_network_and_with_the_switch_on() {
    let (hub, shared, _stream, home) = hub("show-page-api");
    hub.host_register(&register_params("a"))
        .expect("registered");
    // the App notes the pane's terminal when it resolves the caller
    hub.note_pane_terminal("w2:pD", "term-7");
    let file = home.join("board.html");
    std::fs::write(&file, "<button>go</button>").unwrap();
    let show = |page_api: bool, allow_network: bool| {
        hub.run(
            &pane("w2:pD"),
            params(BrowserOp::Show {
                file: file.display().to_string(),
                allow_network,
                page_api,
                watch: false,
                focus: false,
                title: None,
            }),
        )
    };
    let last_show = || {
        let state = shared.lock().unwrap();
        state
            .ops
            .iter()
            .rev()
            .find(|(op, _, _)| op == "show")
            .map(|op| op.2.clone())
            .unwrap()
    };
    let shown = show(true, false).expect("show");
    assert_eq!(
        shown.data["page_api"], true,
        "{:?} {}",
        shown.data, shown.text
    );
    assert!(shown.text.contains("window.herdr.send"), "{}", shown.text);
    assert_eq!(last_show()["page_api"], true);
    assert_eq!(
        last_show()["opener"],
        hub.opener_token("term-7").as_str(),
        "who showed it, for the companion to keep on the entry"
    );
    let tab = shown.tab.clone().expect("a tab");
    let target = hub
        .with_state_mut(|state| {
            state
                .resolve_tab("companion", &tab)
                .map(|r| r.target_id.clone())
        })
        .expect("the ledger knows the view");
    assert!(
        hub.agent_driving(&target),
        "the agent's own show drives the page for the grace"
    );
    let refused = show(true, true).unwrap_err();
    assert_eq!(refused.code, "invalid_request");
    let mut config = hub.config();
    config.page_api = false;
    hub.apply_config(&config);
    let off = show(true, false).expect("show without the bridge");
    assert!(off.data.get("page_api").is_none());
    assert!(off.text.contains("page messages are off"), "{}", off.text);
    assert_eq!(last_show()["page_api"], false);
    assert!(
        last_show()["opener"].is_null(),
        "no opener without the bridge"
    );
    config.page_api = true;
    hub.apply_config(&config);
    let unknown = hub
        .run(
            &pane("w2:pE"),
            params(BrowserOp::Show {
                file: file.display().to_string(),
                allow_network: false,
                page_api: true,
                watch: false,
                focus: false,
                title: None,
            }),
        )
        .expect("show");
    assert!(
        unknown.data.get("page_api").is_none(),
        "an unknown opener gets no bridge"
    );
    let plain = show(false, false).expect("show");
    assert!(!plain.text.contains("window.herdr"));
}

/// Audit A1/A8: only an agent op that fires page script or trusted input
/// (eval, act, navigate, history, dialog) makes a companion page "driven";
/// an agent's read or wait never does, a user's op never does.
#[test]
fn only_an_agents_eval_or_act_drives_a_companion_page() {
    let (hub, _shared, _stream, home) = hub("agent-driving");
    hub.host_register(&register_params("a"))
        .expect("registered");
    let file = home.join("p.html");
    std::fs::write(&file, "<p>p</p>").unwrap();
    let shown = hub
        .run(
            &BrowserActor::User,
            params(BrowserOp::Show {
                file: file.display().to_string(),
                allow_network: false,
                page_api: false,
                watch: false,
                focus: false,
                title: None,
            }),
        )
        .expect("a user's show");
    let tab = shown.tab.clone().expect("a tab");
    let target = hub
        .with_state_mut(|state| {
            state
                .resolve_tab("companion", &tab)
                .map(|r| r.target_id.clone())
        })
        .expect("the ledger knows the view");
    assert!(
        !hub.agent_driving(&target),
        "a user's show is not an agent's"
    );
    let on_tab = |op: BrowserOp| BrowserRunParams {
        tab: Some(tab.clone()),
        ..params(op)
    };
    let agent = pane("w2:pD");
    hub.run(
        &agent,
        on_tab(BrowserOp::Read {
            format: None,
            selector: None,
            ref_: None,
            offset: None,
            max: None,
            all: false,
            interactive: false,
        }),
    )
    .expect("read");
    hub.run(
        &agent,
        on_tab(BrowserOp::Wait {
            text: None,
            gone: None,
            selector: None,
            url: None,
            load: Some("load".into()),
            timeout_s: Some(1),
        }),
    )
    .expect("wait");
    assert!(
        !hub.agent_driving(&target),
        "an agent's read or wait drives nothing"
    );
    hub.run(
        &BrowserActor::User,
        on_tab(BrowserOp::Eval {
            expr: "1".into(),
            max: None,
        }),
    )
    .expect("a user's eval");
    assert!(!hub.agent_driving(&target), "a user's eval is the user's");
    hub.run(
        &agent,
        on_tab(BrowserOp::Eval {
            expr: "setTimeout(() => window.herdr.send({text: 'x'}), 1500)".into(),
            max: None,
        }),
    )
    .expect("an agent's eval");
    assert!(
        hub.agent_driving(&target),
        "an agent's eval drives the page"
    );
    std::thread::sleep(Duration::from_millis(1_500));
    assert!(
        hub.agent_driving(&target),
        "its setTimeout(1500) send still lands inside the grace"
    );
}

#[test]
fn a_supervisor_pass_never_drops_a_live_lease() {
    let (hub, _shared, _stream, _home) = hub("companion-expiry");
    hub.install_test_lease("L1");
    hub.supervise();
    assert!(hub.companion_live(), "a live lease stays");
    hub.expire_host_lease_for_test();
    hub.supervise();
    assert!(!hub.companion_live(), "an expired one goes");
}

// ----- Fork (companion launcher): the herdr+ window off ----------------------

/// A hub with the defaults (window off, host companion) and the fake sidecar.
fn window_off_hub(name: &str) -> (BrowserHub, Shared, UnixStream, PathBuf) {
    let home = temp_home(name);
    let hub = BrowserHub::with_home(BrowserConfig::default(), home.clone());
    let (shared, stream) = fake_host(&hub);
    (hub, shared, stream, home)
}

const WINDOW_OFF: &str = "the herdr+ browser window is off ([browser] window = false); agents use the companion browser. Set [browser] window = true to use the window again.";

#[test]
fn fork_smoke_with_the_window_off_window_profiles_answer_window_disabled_with_the_fix() {
    let (hub, shared, _stream, home) = window_off_hub("window-off");
    let actor = pane("w2:pD");
    // a window profile, named or through a qualified tab
    let named = BrowserRunParams {
        profile: Some("main".into()),
        ..params(open_op("https://a.test/"))
    };
    let err = hub.run(&actor, named).unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        ("window_disabled", WINDOW_OFF)
    );
    let qualified = BrowserRunParams {
        tab: Some("main:t3".into()),
        ..params(BrowserOp::Read {
            format: None,
            selector: None,
            ref_: None,
            offset: None,
            max: None,
            all: false,
            interactive: false,
        })
    };
    assert_eq!(
        hub.run(&actor, qualified).unwrap_err().code,
        "window_disabled"
    );
    // `new` would create a temporary window profile: refused before that
    let fresh = BrowserRunParams {
        profile: Some("new".into()),
        ..params(open_op("https://a.test/"))
    };
    assert_eq!(hub.run(&actor, fresh).unwrap_err().code, "window_disabled");
    assert!(
        hub.profiles().iter().all(|p| !p.temporary),
        "no profile was made"
    );
    assert_eq!(hub.start(Some("main")).unwrap_err().code, "window_disabled");
    // an open without a profile goes to the companion: not installed here
    let err = hub
        .run(&actor, params(open_op("https://a.test/")))
        .unwrap_err();
    assert_eq!(err.code, "host_unavailable");
    assert!(
        err.message.contains("herdr browser setup --companion"),
        "{}",
        err.message
    );
    // `auto` falls back only to an enabled window: the companion too
    hub.apply_config(&BrowserConfig {
        host: "auto".into(),
        ..BrowserConfig::default()
    });
    let err = hub
        .run(&actor, params(open_op("https://a.test/")))
        .unwrap_err();
    assert_eq!(err.code, "host_unavailable");
    // `window` without the window: window_disabled, and a config diagnostic
    let window_host = BrowserConfig {
        host: "window".into(),
        ..BrowserConfig::default()
    };
    assert!(window_host
        .diagnostics()
        .iter()
        .any(|d| d.contains("needs [browser] window = true")));
    hub.apply_config(&window_host);
    let err = hub
        .run(&actor, params(open_op("https://a.test/")))
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        ("window_disabled", WINDOW_OFF)
    );
    // nothing ever reached a window profile, and Chromium was never launched
    let ops = shared.lock().unwrap().ops.clone();
    assert!(
        ops.iter().all(|(op, _, _)| op != "attach"),
        "no window attach: {ops:?}"
    );
    // the server says so to clients, and the doctor does not ask for Chromium
    assert!(hub.get(None).window_off);
    let env = crate::browser::setup::SetupEnv {
        browser_home: home.clone(),
        binary: PathBuf::from("/usr/bin/true"),
        shell_file: home.join("shell.zsh"),
        zshrc: None,
        claude_json: None,
        claude_bin: None,
        claude_z_bin: None,
        codex_config: None,
        codex_bin: None,
        home: Some(home.join("nohome")),
        node_override: None,
    };
    let checks = crate::browser::setup::checks(&BrowserConfig::default(), &env, None);
    for id in ["executable", "extension"] {
        let check = checks.iter().find(|c| c.id == id).unwrap();
        assert!(check.ok, "{check:?}");
        assert_eq!(check.detail, "not used ([browser] window = false)");
    }
    // `window = true` brings it back exactly
    hub.apply_config(&window_config());
    hub.set_profile_running_for_test("main", 0, 4321);
    let opened = hub
        .run(&actor, params(open_op("https://a.test/")))
        .expect("the window again");
    assert!(opened.tab.as_deref().unwrap().starts_with("main:"));
    assert!(!hub.get(None).window_off);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn with_the_window_off_a_cursor_on_a_window_tab_is_ignored() {
    let (hub, _shared, _stream, home) = hub("window-off-cursor");
    let actor = pane("w2:pD");
    hub.run(&actor, params(open_op("https://a.test/")))
        .expect("an open in the window");
    let read = || {
        params(BrowserOp::Read {
            format: None,
            selector: None,
            ref_: None,
            offset: None,
            max: None,
            all: false,
            interactive: false,
        })
    };
    hub.run(&actor, read()).expect("its cursor is on main");
    // the window goes off: the cursor no longer picks a profile
    hub.apply_config(&BrowserConfig::default());
    let err = hub.run(&actor, read()).unwrap_err();
    assert_eq!(err.code, "no_current_tab", "{err:?}");
    // a pane without any cursor: the same, nothing starts
    let err = hub.run(&pane("w9:p9"), read()).unwrap_err();
    assert_eq!(err.code, "no_current_tab");
    let _ = std::fs::remove_dir_all(&home);
}
