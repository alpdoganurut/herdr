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
struct FakeHostState {
    ops: Vec<(String, Option<String>, Value)>,
    /// Ops the host must never answer.
    silent: Vec<String>,
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
}

type Shared = Arc<Mutex<FakeHostState>>;

fn fake_host(hub: &BrowserHub) -> (Shared, UnixStream) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let shared: Shared = Arc::new(Mutex::new(FakeHostState {
        next_target: 1,
        ..Default::default()
    }));
    let state = shared.clone();
    let mut writer = theirs.try_clone().unwrap();
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
                            events.push(json!({ "event": "tab", "profile": "main", "target": target, "kind": "opened", "url": "about:blank", "initiator": "other" }).to_string());
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
                                json!({ "id": id, "ok": true, "result": { "kind": kind, "role": if kind == "click" { "button" } else { "textbox" }, "name": if kind == "click" { "Press me" } else { "Name" }, "navigated": kind == "click", "url_before": "https://site.test/T1" }, "page": { "url": "https://site.test/after", "title": "After", "dialog_open": false } })
                            }
                        }
                        "navigate" | "history" | "focus" => {
                            json!({ "id": id, "ok": true, "result": {}, "page": { "url": args["url"].as_str().unwrap_or("https://site.test/after"), "title": "After", "status": 200 } })
                        }
                        "close" | "close_browser" => json!({ "id": id, "ok": true, "result": {} }),
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
                writer.write_all(event.as_bytes()).unwrap();
                writer.write_all(b"\n").unwrap();
            }
            if let Some(reply) = reply {
                let mut writer = writer.try_clone().unwrap();
                std::thread::spawn(move || {
                    if delay > 0 {
                        std::thread::sleep(Duration::from_millis(delay));
                    }
                    let _ = writer.write_all(reply.to_string().as_bytes());
                    let _ = writer.write_all(b"\n");
                });
            }
        }
    });
    let reader: Box<dyn BufRead + Send> = Box::new(BufReader::new(ours.try_clone().unwrap()));
    hub.install_fake_host(Box::new(ours.try_clone().unwrap()), reader);
    (shared, ours)
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("herdr-browser-hub-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn hub(name: &str) -> (BrowserHub, Shared, UnixStream, PathBuf) {
    let home = temp_home(name);
    let hub = BrowserHub::with_home(BrowserConfig::default(), home.clone());
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
    let again = BrowserHub::with_home(BrowserConfig::default(), home.clone());
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
        ..BrowserConfig::default()
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
        ..BrowserConfig::default()
    });
    assert_eq!(
        hub.run(&actor, params(BrowserOp::Tabs { mine: false }))
            .unwrap_err()
            .code,
        "browser_disabled"
    );
    assert!(!hub.get(None).enabled);
    hub.apply_config(&BrowserConfig::default());
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
    // a live SingletonLock pid herdr did not record
    std::os::unix::fs::symlink(
        format!("host-{}", std::process::id()),
        store.dir("scratch").join("SingletonLock"),
    )
    .unwrap();
    let err = hub.profile_delete("scratch").unwrap_err();
    assert_eq!(err.code, "profile_running");
    assert!(err.message.contains("still holds"));
    std::fs::remove_file(store.dir("scratch").join("SingletonLock")).unwrap();
    // a stale run record with a live pid
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
    assert_eq!(
        hub.profile_delete("scratch").unwrap_err().code,
        "profile_running"
    );
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
        ..BrowserConfig::default()
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
        ..BrowserConfig::default()
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
        ..BrowserConfig::default()
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
    // Long code is clipped in the ledger.
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
        assert!(
            detail.starts_with("eval: document.title + 'yyyy"),
            "{detail}"
        );
        assert!(detail.chars().count() <= 220, "{}", detail.chars().count());
    });
    // Passwords allowed by config: no guard.
    hub.apply_config(&BrowserConfig {
        type_into_password_fields: true,
        ..BrowserConfig::default()
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
        ..BrowserConfig::default()
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
        ..BrowserConfig::default()
    });
    hub.apply_config(&BrowserConfig {
        show_activity: false,
        ..BrowserConfig::default()
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
