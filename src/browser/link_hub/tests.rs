//! The linked browser in the hub, against a Mac without a socket
//! ([`FakeMac`]) and, beside it, the fake local sidecar.

use std::time::{Duration, Instant};

use serde_json::json;

use super::super::{BrowserHub, TabKey};
use crate::api::schema::{
    BrowserActor, BrowserHostCdp, BrowserHostKind, BrowserHostRegisterParams, BrowserOp,
    BrowserRunParams, BrowserRunResult,
};
use crate::browser::companion_app::tests::{companion_hub, FakeSpawner};
use crate::browser::companion_app::StartReason;
use crate::browser::link::tests::fake_mac::FakeMac;
use crate::browser::link::tests::mac_params;
use crate::browser::link::LinkEnd;
use crate::browser::link::LinkHandler as _;
use crate::browser::link_proto::{
    BrowserLinkOpenParams, BrowserLinkReady, Bye, HelperFrame, LinkError, LinkReply, OPS_IN_FLIGHT,
};
use crate::browser::tests::Shared;
use crate::browser::BrowserError;
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

fn run(hub: &BrowserHub, op: BrowserOp) -> Result<BrowserRunResult, BrowserError> {
    hub.run(
        &pane("w2:pD"),
        BrowserRunParams {
            caller: None,
            profile: None,
            tab: None,
            op,
            timeout_ms: None,
        },
    )
}

fn open(hub: &BrowserHub) -> Result<BrowserRunResult, BrowserError> {
    run(
        hub,
        BrowserOp::Open {
            url: "https://a.test/".into(),
            focus: false,
            wait: None,
        },
    )
}

fn click(hub: &BrowserHub) -> Result<BrowserRunResult, BrowserError> {
    run(
        hub,
        serde_json::from_value(json!({ "op": "click", "ref": "e1" })).unwrap(),
    )
}

fn read(hub: &BrowserHub) -> Result<BrowserRunResult, BrowserError> {
    run(
        hub,
        serde_json::from_value(json!({ "op": "read", "offset": 0 })).unwrap(),
    )
}

/// A hub (companion not installed), its fake sidecar, and a Mac that serves
/// no page yet.
fn linked(name: &str) -> (BrowserHub, Shared, FakeMac) {
    let (hub, _home, shared) = companion_hub(name, false);
    let mac = FakeMac::open(&hub, "mac-a", "mac").expect("linked");
    mac.up("i1", &[]);
    wait_for("its lease", || !mac.state.lock().unwrap().leases.is_empty());
    (hub, shared, mac)
}

fn sidecar_ops(shared: &Shared) -> Vec<String> {
    let state = shared.lock().unwrap();
    state.ops.iter().map(|(op, _, _)| op.clone()).collect()
}

fn register(instance: &str) -> BrowserHostRegisterParams {
    BrowserHostRegisterParams {
        kind: BrowserHostKind::Companion,
        instance: instance.into(),
        pid: Some(4242),
        cdp: BrowserHostCdp {
            host: "127.0.0.1".into(),
            port: 49731,
        },
        nonce: "n".into(),
        version: "0.2.0".into(),
        protocol: 1,
        capabilities: vec!["views".into()],
        launch_token: None,
        managed: false,
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn fork_smoke_a_linked_browser_takes_the_agents_ops_and_the_local_sidecar_none() {
    let (hub, shared, mac) = linked("link-route");
    assert!(hub.companion_live());
    mac.say(&HelperFrame::Ping { n: 7 });
    let opened = open(&hub).expect("open");
    assert_eq!(opened.tab.as_deref(), Some("companion:t1"));
    assert_eq!(opened.data["host"], "companion");
    click(&hub).expect("click");
    read(&hub).expect("read");
    assert_eq!(mac.op_names(), ["open", "act", "read"]);
    let state = mac.state.lock().unwrap();
    let op = &state.ops[0];
    assert_eq!(op.args["url"], "https://a.test/");
    assert_eq!(op.args["tab_id"], "w2:tD");
    assert_eq!(op.args["opened_by"], "agent:w2:pD");
    assert_eq!(op.echo, 7, "the last ping it saw");
    assert!(op
        .activity
        .as_ref()
        .is_some_and(|a| a["frame"].is_boolean()));
    assert_eq!(state.ops[1].target.as_deref(), Some("V1"));
    assert!(state.ensures.is_empty(), "it was up");
    assert_eq!(sidecar_ops(&shared), Vec::<String>::new());
}

#[test]
fn a_link_drop_in_the_middle_of_an_op_says_whether_it_may_have_happened() {
    let (hub, _shared, mac) = linked("link-drop");
    open(&hub).expect("open");
    mac.state.lock().unwrap().silent = vec!["act".into(), "read".into()];
    let calls = [click as fn(&BrowserHub) -> _, read].map(|call| {
        let hub = hub.clone();
        std::thread::spawn(move || call(&hub))
    });
    mac.got("act");
    mac.got("read");
    let dropped = Instant::now();
    mac.close(LinkEnd::Lost("test".into()));
    let [acted, looked] = calls.map(|call| call.join().unwrap().unwrap_err());
    assert!(dropped.elapsed() < Duration::from_secs(1));
    assert_eq!(acted.code, "host_link_lost");
    assert_eq!(acted.message, "the connection to mac dropped while act ran: it may or may not have happened. Check with browser_read or browser_snapshot before repeating it.");
    assert_eq!(looked.code, "host_link_lost");
    assert_eq!(
        looked.message,
        "the connection to mac dropped while read ran; it changed nothing, call it again"
    );
    // the lease lives on for a while: nothing is sent, nothing starts here
    let spawner = FakeSpawner::new(&hub, true);
    for call in [open, click, read] {
        let err = call(&hub).unwrap_err();
        assert_eq!(err.code, "host_unavailable");
        assert_eq!(err.message, "the connection to the browser on mac dropped and is reconnecting; nothing was sent; call again in a few seconds");
    }
    hub.with_state(|state| {
        assert!(state.tabs.values().all(|record| record.is_open()));
    });
    // then it is gone, and its pages with it
    hub.expire_host_lease_for_test();
    hub.supervise();
    let err = open(&hub).unwrap_err();
    assert_eq!(err.code, "host_unavailable");
    assert_eq!(err.message, "no browser: the user's herdr+ client is not connected (last: mac, just now). Nothing was sent. Continue without the browser, or ask the user to open herdr+ there.");
    hub.with_state(|state| {
        assert!(state.tabs.values().all(|record| !record.is_open()));
    });
    assert_eq!(spawner.spawns(), 0);
    let mut sent = mac.op_names();
    sent.sort();
    assert_eq!(sent, ["act", "open", "read"], "nothing was retried");
}

#[test]
fn the_same_mac_comes_back_to_the_same_tabs_and_cursors() {
    let (hub, _shared, mac) = linked("link-back");
    open(&hub).expect("open");
    let lease = mac.state.lock().unwrap().leases[0].clone();
    assert!(hub.lease_matches(&lease));
    hub.accept_host_sync_seq(&lease, 5).unwrap();
    mac.close(LinkEnd::Lost("test".into()));
    let back = FakeMac::open(&hub, "mac-a", "mac").expect("back");
    back.up("i1", &["V1"]);
    wait_for("host_ok", || !back.state.lock().unwrap().leases.is_empty());
    // one lease: the companion's sync sequence goes on
    assert_eq!(back.state.lock().unwrap().leases[0], lease);
    assert_eq!(
        hub.accept_host_sync_seq(&lease, 5).unwrap_err().code,
        "stale_seq"
    );
    let read_back = read(&hub).expect("the pane's cursor is still on it");
    assert_eq!(read_back.tab.as_deref(), Some("companion:t1"));
    assert_eq!(
        back.state.lock().unwrap().ops[0].target.as_deref(),
        Some("V1")
    );
    // also after the lease ran out: the record is reopened, not a new one
    back.close(LinkEnd::Silent);
    hub.expire_host_lease_for_test();
    hub.supervise();
    let again = FakeMac::open(&hub, "mac-a", "mac").expect("again");
    again.up("i1", &["V1"]);
    hub.with_state(|state| {
        let record = &state.tabs[&TabKey::new("companion", "V1")];
        assert!(record.is_open());
        assert_eq!(record.short, "t1");
    });
    wait_for("host_ok", || !again.state.lock().unwrap().leases.is_empty());
    assert_ne!(again.state.lock().unwrap().leases[0], lease);
}

#[test]
fn a_new_companion_there_moves_its_pages_by_entry() {
    let (hub, _shared, mac) = linked("link-new-instance");
    open(&hub).expect("open");
    let lease = mac.state.lock().unwrap().leases[0].clone();
    hub.bind_companion_entries(&[("V1".into(), "g-1/b1".into())]);
    mac.say(&HelperFrame::HostDown {
        reason: "crashed".into(),
    });
    mac.up("i2", &["W9"]);
    wait_for("a second lease", || {
        mac.state.lock().unwrap().leases.len() == 2
    });
    assert!(
        !hub.lease_matches(&lease),
        "another companion, another lease"
    );
    hub.bind_companion_entries(&[("W9".into(), "g-1/b1".into())]);
    let read_back = read(&hub).expect("the cursor moved with its page");
    assert_eq!(read_back.tab.as_deref(), Some("companion:t1"));
    assert_eq!(
        mac.state
            .lock()
            .unwrap()
            .ops
            .last()
            .unwrap()
            .target
            .as_deref(),
        Some("W9")
    );
}

#[test]
fn what_an_old_epoch_says_is_dropped() {
    let (hub, _shared, old) = linked("link-epoch");
    open(&hub).expect("open");
    old.state.lock().unwrap().silent = vec!["read".into()];
    let pending = {
        let hub = hub.clone();
        std::thread::spawn(move || read(&hub))
    };
    old.got("read");
    let new = FakeMac::open(&hub, "mac-a", "mac").expect("replaced");
    assert_eq!(new.link.epoch, old.link.epoch + 1);
    assert_eq!(pending.join().unwrap().unwrap_err().code, "host_link_lost");
    // nothing goes over the new link before its browser is up there
    new.state.lock().unwrap().tabs = vec!["V1".into()];
    read(&hub).expect("asked for, then read");
    assert_eq!(new.op_names(), ["read"]);
    assert_eq!(new.state.lock().unwrap().ensures.len(), 1);
    new.up("i1", &["V1"]);
    wait_for("the new link's lease", || {
        !new.state.lock().unwrap().leases.is_empty()
    });
    wait_for("the old link's goodbye", || {
        old.state.lock().unwrap().byes == ["replaced"]
    });
    // its frames, also straight to the hub, change nothing
    let id = old.state.lock().unwrap().ops.last().unwrap().id;
    old.say(&HelperFrame::Re(LinkReply {
        id,
        ok: true,
        result: json!({}),
        page: None,
        error: None,
    }));
    hub.host_down(&old.link, "late");
    hub.event(
        &old.link,
        json!({ "event": "tab", "target": "V1", "kind": "closed" }),
    );
    hub.host_up(
        &old.link,
        serde_json::from_value(json!({ "instance": "i0", "tabs": [] })).unwrap(),
    );
    old.close(LinkEnd::Replaced);
    hub.with_state(|state| {
        assert!(state.tabs[&TabKey::new("companion", "V1")].is_open());
    });
    assert_eq!(
        read(&hub).expect("the new link").tab.as_deref(),
        Some("companion:t1")
    );
    // a goodbye of the old one does not take the new one's lease either
    old.close(LinkEnd::Bye(Bye {
        code: "quit".into(),
        message: String::new(),
    }));
    read(&hub).expect("still there");
}

#[test]
fn a_local_sidecar_that_goes_does_not_touch_the_link() {
    let (hub, shared, mac) = linked("link-vs-local");
    open(&hub).expect("open");
    // an id of the local sidecar's is nothing to the link and the other way
    mac.state.lock().unwrap().silent = vec!["read".into()];
    let pending = {
        let hub = hub.clone();
        std::thread::spawn(move || read(&hub))
    };
    mac.got("read");
    let id = mac.state.lock().unwrap().ops.last().unwrap().id;
    hub.test_local_reply(id);
    // what the local sidecar says about its companion, then its end
    hub.test_local_event(json!({
        "event": "browser", "profile": "companion", "kind": "disconnected", "detail": "x"
    }));
    hub.drop_fake_host_for_test();
    assert!(hub.companion_live());
    assert!(!pending.is_finished(), "still waiting for the Mac");
    mac.say(&HelperFrame::Re(LinkReply {
        id,
        ok: true,
        result: json!({ "content": "from the Mac", "total_chars": 12 }),
        page: None,
        error: None,
    }));
    let result = pending.join().unwrap().expect("the Mac's answer");
    assert!(result.text.contains("from the Mac"), "{}", result.text);
    assert_eq!(sidecar_ops(&shared), Vec::<String>::new());
}

#[test]
fn the_browser_there_is_started_by_the_op_that_needs_it() {
    let (hub, _shared, mac) = linked("link-ensure");
    mac.say(&HelperFrame::HostDown {
        reason: "stopped".into(),
    });
    assert!(!hub.companion_live());
    let calls: Vec<_> = (0..4)
        .map(|_| {
            let hub = hub.clone();
            std::thread::spawn(move || open(&hub))
        })
        .collect();
    for call in calls {
        call.join().unwrap().expect("started, then opened");
    }
    let state = mac.state.lock().unwrap();
    assert!(!state.ensures.is_empty() && state.ensures.len() <= 4);
    assert_eq!(state.ensures[0].reason, "open");
    drop(state);
    // it cannot start
    mac.say(&HelperFrame::HostDown {
        reason: "stopped".into(),
    });
    mac.state.lock().unwrap().ensure_error = Some(LinkError::new(
        "host_unavailable",
        "gave up after 5 crashes",
    ));
    let err = open(&hub).unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        (
            "host_unavailable",
            "the browser on mac did not start (gave up after 5 crashes); nothing was sent"
        )
    );
    // the link drops while it starts
    {
        let mut state = mac.state.lock().unwrap();
        state.ensure_error = None;
        state.ensure_silent = true;
        state.ensures.clear();
    }
    let pending = {
        let hub = hub.clone();
        std::thread::spawn(move || open(&hub))
    };
    wait_for("the ensure", || {
        !mac.state.lock().unwrap().ensures.is_empty()
    });
    mac.close(LinkEnd::Lost("test".into()));
    let err = pending.join().unwrap().unwrap_err();
    assert_eq!(err.code, "host_unavailable");
    assert!(err.message.contains("is reconnecting"), "{}", err.message);
}

#[test]
fn a_mac_without_the_browser_is_named_with_its_fix() {
    let cases = [
        (
            BrowserLinkReady::NotInstalled,
            None,
            "the browser is not installed on mac; ask the user to run \"herdr browser setup --companion\" there",
        ),
        (
            BrowserLinkReady::Failed,
            Some("no Electron"),
            "the browser on mac did not start (no Electron); nothing was sent",
        ),
        (
            BrowserLinkReady::Unknown,
            None,
            "the browser on mac did not start; nothing was sent",
        ),
    ];
    for (at, (ready, detail, text)) in cases.into_iter().enumerate() {
        // the companion is installed here: it is not started instead
        let (hub, _home, _shared) = companion_hub(&format!("link-ready-{at}"), true);
        let spawner = FakeSpawner::new(&hub, true);
        let mac = FakeMac::open_with(
            &hub,
            BrowserLinkOpenParams {
                ready,
                detail: detail.map(str::to_string),
                ..mac_params("mac-a", "mac")
            },
        )
        .unwrap();
        for call in [open, read] {
            let err = call(&hub);
            // a read without a page of its own asks for nothing
            if err.as_ref().is_err_and(|err| err.code == "no_current_tab") {
                continue;
            }
            let err = err.unwrap_err();
            assert_eq!(
                (err.code.as_str(), err.message.as_str()),
                ("host_unavailable", text)
            );
        }
        assert!(mac.state.lock().unwrap().ensures.is_empty());
        assert_eq!(spawner.spawns(), 0);
    }
}

#[test]
fn only_the_ops_of_the_table_are_ever_sent() {
    let (hub, shared, mac) = linked("link-ops");
    open(&hub).expect("open");
    let sent = mac.op_names();
    for op in [
        "dashboard",
        "ntp",
        "show",
        "watch",
        "unwatch",
        "attach",
        "detach",
        "close_browser",
        "control_quit",
        "hello",
        "ping",
        "upload",
    ] {
        let err = hub
            .request(
                op,
                Some("companion"),
                Some("V1"),
                json!({}),
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(err.code, "host_refused", "{op}");
        assert_eq!(
            err.message,
            format!("mac refuses {op} for remote machines (only herdr+ on that machine may do it)")
        );
    }
    // the housekeeping that goes to a local sidecar
    hub.rewatch_html_views(vec![("V1".into(), "/tmp/x.html".into())]);
    hub.schedule_ntp_push();
    let mut pinned = hub.config();
    pinned.pin_dashboard = !pinned.pin_dashboard;
    hub.apply_config(&pinned);
    hub.supervise();
    assert_eq!(hub.companion_stop().unwrap_err().code, "host_in_use");
    wait_for("the sidecar's housekeeping", || {
        sidecar_ops(&shared).contains(&"dashboard".to_string())
    });
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(mac.op_names(), sent);
    assert!(!sidecar_ops(&shared)
        .iter()
        .any(|op| op == "watch" || op == "attach"));
}

#[test]
fn an_event_is_the_companion_profiles_whatever_it_says() {
    let (hub, _shared, mac) = linked("link-forced");
    hub.set_profile_running_for_test("main", 0, 4321);
    for profile in [json!("main"), json!(null), json!(7)] {
        mac.say(&HelperFrame::Ev {
            event: json!({
                "event": "tab", "profile": profile, "target": "E1", "kind": "opened",
                "url": "https://evil.test/", "initiator": "other"
            }),
        });
        hub.with_state(|state| {
            let profiles: Vec<_> = state.tabs.values().map(|r| r.profile.clone()).collect();
            assert_eq!(profiles, ["companion"]);
        });
    }
    // a browser event and noise end nothing
    for event in [
        json!({ "event": "browser", "profile": "main", "kind": "disconnected", "detail": "" }),
        json!({ "event": "from_the_future" }),
        json!({ "event": "tab" }),
        json!("tab"),
    ] {
        assert_eq!(mac.say(&HelperFrame::Ev { event }), None);
    }
    assert!(hub.companion_live());
    hub.with_state(|state| assert!(state.profile("main").is_running()));
}

#[test]
fn a_link_and_a_local_companion_never_both_serve() {
    let (hub, _shared, mac) = linked("link-exclusive");
    for instance in ["a", "i1"] {
        let err = hub.host_register(&register(instance)).unwrap_err();
        assert_eq!(err.code, "host_in_use");
        assert_eq!(err.message, "the browser runs on mac; control it there");
    }
    // its lease is the link's alone
    let lease = mac.state.lock().unwrap().leases[0].clone();
    assert_eq!(
        hub.host_heartbeat(&lease).unwrap_err().code,
        "lease_unknown"
    );
    assert_eq!(
        hub.host_unregister(&lease).unwrap_err().code,
        "lease_unknown"
    );
    assert!(hub.companion_live());
    for control in [
        hub.companion_start(StartReason::User).map(|()| None),
        hub.companion_stop(),
        hub.companion_restart(StartReason::User),
    ] {
        let err = control.unwrap_err();
        assert_eq!(err.message, "the browser runs on mac; control it there");
    }
    // another Mac, linked and while the first reconnects
    let refused = |hub: &BrowserHub| {
        let err = FakeMac::open(hub, "mac-b", "other")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code, "link_in_use");
        err.message
    };
    let text = "the browser of devhost1 is on mac (since 14:02); quit herdr+ there";
    assert_eq!(refused(&hub), text);
    mac.close(LinkEnd::Lost("test".into()));
    assert_eq!(refused(&hub), text);
    assert_eq!(
        hub.host_register(&register("a")).unwrap_err().code,
        "host_in_use"
    );
    // once it is gone, the local one may
    hub.expire_host_lease_for_test();
    hub.supervise();
    hub.host_register(&register("a")).expect("registered");
    let err = FakeMac::open(&hub, "mac-a", "mac").map(|_| ()).unwrap_err();
    assert_eq!(err.code, "link_in_use");
    // and one that is still attaching holds it too
    hub.expire_host_lease_for_test();
    hub.supervise();
    hub.install_pending_registration_for_test("a", Duration::ZERO);
    let err = FakeMac::open(&hub, "mac-a", "mac").map(|_| ()).unwrap_err();
    assert_eq!(err.code, "link_in_use");
}

#[test]
fn the_helpers_goodbye_ends_the_lease_at_once() {
    let (hub, _shared, mac) = linked("link-quit");
    open(&hub).expect("open");
    mac.close(LinkEnd::Bye(Bye {
        code: "quit".into(),
        message: String::new(),
    }));
    assert!(!hub.companion_live());
    let err = open(&hub).unwrap_err();
    assert!(err
        .message
        .starts_with("no browser: the user's herdr+ client is not connected (last: mac, "));
    // another Mac need not wait
    FakeMac::open(&hub, "mac-b", "other").expect("free");
}

#[test]
fn pings_keep_the_lease_and_silence_ends_it() {
    let (hub, _shared, mac) = linked("link-renew");
    hub.expire_host_lease_for_test();
    assert!(!hub.companion_live());
    mac.say(&HelperFrame::Ping { n: 1 });
    assert!(hub.companion_live(), "renewed before the supervisor looked");
    hub.expire_host_lease_for_test();
    hub.supervise();
    assert!(!hub.companion_live());
    mac.say(&HelperFrame::Ping { n: 2 });
    assert!(
        !hub.companion_live(),
        "a dropped lease is not a ping's to bring back"
    );
    open(&hub).expect("but the next op is");
}

#[test]
fn one_more_op_than_may_run_is_told_so() {
    let (hub, _shared, mac) = linked("link-busy");
    open(&hub).expect("open");
    mac.state.lock().unwrap().silent = vec!["read".into()];
    let ask = |hub: &BrowserHub| {
        hub.request(
            "read",
            Some("companion"),
            Some("V1"),
            json!({}),
            Duration::from_secs(20),
        )
    };
    let calls: Vec<_> = (0..OPS_IN_FLIGHT)
        .map(|_| {
            let hub = hub.clone();
            std::thread::spawn(move || ask(&hub))
        })
        .collect();
    wait_for("32 ops", || {
        mac.op_names().iter().filter(|op| *op == "read").count() == OPS_IN_FLIGHT
    });
    let err = ask(&hub).unwrap_err();
    assert_eq!(err.code, "host_busy");
    assert_eq!(err.message, "32 browser ops are already running");
    mac.close(LinkEnd::Lost("test".into()));
    for call in calls {
        assert_eq!(call.join().unwrap().unwrap_err().code, "host_link_lost");
    }
}

#[test]
fn an_answer_that_never_comes_is_a_timeout_and_frees_its_place() {
    let (hub, _shared, mac) = linked("link-timeout");
    mac.state.lock().unwrap().silent = vec!["read".into()];
    let started = Instant::now();
    let err = hub
        .request(
            "read",
            Some("companion"),
            Some("V1"),
            json!({}),
            Duration::from_millis(100),
        )
        .unwrap_err();
    assert_eq!(err.code, "browser_timeout");
    // the deadline, the sidecar's grace and 2 s
    assert!(started.elapsed() >= Duration::from_millis(7_100));
    // its late answer is nobody's
    let id = mac.state.lock().unwrap().ops[0].id;
    let late = HelperFrame::Re(LinkReply {
        id,
        ok: true,
        result: json!({}),
        page: None,
        error: None,
    });
    assert_eq!(mac.say(&late), None);
}

#[test]
fn gone_panes_are_released_over_the_link_64_at_a_time() {
    let (hub, shared, mac) = linked("link-release");
    let panes: Vec<String> = (0..70).map(|n| format!("w1:p{n}")).collect();
    hub.release_panes(panes);
    wait_for("two frames", || mac.op_names() == ["release", "release"]);
    let state = mac.state.lock().unwrap();
    let count = |at: usize| state.ops[at].args["keys"].as_array().unwrap().len();
    assert_eq!((count(0), count(1)), (64, 6));
    assert!(!sidecar_ops(&shared).contains(&"release".to_string()));
}

#[test]
fn the_link_key_says_who_may_serve() {
    let config = |hub: &BrowserHub, link: &str| BrowserConfig {
        link: link.into(),
        ..hub.config()
    };
    // off: no link, and one that is there ends
    let (hub, _shared, mac) = linked("link-key");
    hub.apply_config(&config(&hub, "only"));
    assert_eq!(mac.link.ended(), None);
    hub.apply_config(&config(&hub, "off"));
    assert_eq!(mac.link.ended(), Some(LinkEnd::Off));
    mac.close(LinkEnd::Off);
    let err = FakeMac::open(&hub, "mac-a", "mac").map(|_| ()).unwrap_err();
    assert_eq!(err.code, "link_disabled");
    // only: this machine's companion is never started for an agent
    let (hub, _home, _shared) = companion_hub("link-only", true);
    let spawner = FakeSpawner::new(&hub, true);
    hub.apply_config(&config(&hub, "only"));
    let err = open(&hub).unwrap_err();
    assert_eq!(err.code, "host_unavailable");
    assert_eq!(err.message, "no browser: the user's herdr+ client is not connected. Nothing was sent. Continue without the browser, or ask the user to open herdr+ on their machine.");
    assert_eq!(spawner.spawns(), 0);
    // auto: it is, while no Mac is linked
    hub.apply_config(&config(&hub, "auto"));
    open(&hub).expect("the local companion");
    assert_eq!(spawner.spawns(), 1);
    let err = FakeMac::open(&hub, "mac-a", "mac").map(|_| ()).unwrap_err();
    assert_eq!(err.code, "link_in_use");
}

// ---- files and pictures (SV3)

/// A directory with a page and its script; removed when dropped.
struct Site(std::path::PathBuf);

impl Drop for Site {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn site(name: &str) -> Site {
    let dir = std::env::temp_dir().join(format!("herdr-link-site-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<script src=app.js></script>").unwrap();
    std::fs::write(dir.join("app.js"), "console.log(1)").unwrap();
    Site(std::fs::canonicalize(dir).unwrap())
}

fn show(hub: &BrowserHub, file: &std::path::Path, extra: serde_json::Value) -> BrowserRunResult {
    let mut op = json!({ "op": "show", "file": file.display().to_string() });
    for (key, value) in extra.as_object().unwrap() {
        op[key] = value.clone();
    }
    run(hub, serde_json::from_value(op).unwrap()).expect("shown")
}

fn pull(mac: &FakeMac, token: &str, path: &str) -> crate::browser::link_proto::FileReply {
    let before = mac.state.lock().unwrap().files.len();
    mac.say(&HelperFrame::File(
        crate::browser::link_proto::FileRequest {
            id: 40 + before as u64,
            token: token.into(),
            path: path.into(),
            offset: 0,
        },
    ));
    wait_for("its answer", || {
        mac.state.lock().unwrap().files.len() > before
    });
    mac.state.lock().unwrap().files[before].clone()
}

#[test]
fn fork_smoke_a_file_shown_on_a_linked_browser_loads_from_here_once() {
    let (hub, shared, mac) = linked("link-show");
    let site = site("show");
    let page = site.0.join("index.html");
    let shown = show(&hub, &page, json!({}));
    assert_eq!(
        shown.text,
        "showing index.html (network blocked); loads once; show it again after changes"
    );
    assert_eq!(shown.data["watching"], false);
    assert_eq!(shown.data["file"], page.display().to_string());
    assert_eq!(shown.data["kind"], "html_view");
    let op = mac.state.lock().unwrap().ops.last().unwrap().clone();
    assert_eq!(op.op, "show_remote");
    let token = op.args["token"].as_str().unwrap().to_string();
    assert_eq!(op.args["rel"], "index.html");
    assert_eq!(op.args["opened_by"], "agent:w2:pD");
    let sent = serde_json::to_string(&op).unwrap();
    assert!(
        !sent.contains(site.0.to_str().unwrap()),
        "no path of this machine: {sent}"
    );
    assert!(sidecar_ops(&shared).is_empty());
    // the page and what it loads come from here
    let script = pull(&mac, &token, "app.js");
    assert_eq!(
        (script.status, script.data.as_slice()),
        (200, &b"console.log(1)"[..])
    );
    assert_eq!(pull(&mac, &token, "..%2fapp.js").status, 403);
    assert_eq!(pull(&mac, &token, "gone.js").status, 404);
    // its page is the file here, in events and answers
    mac.say(&HelperFrame::Ev {
        event: json!({
            "event": "tab", "target": "V1", "kind": "navigated",
            "url": format!("herdr-file://{token}/index.html"), "initiator": "other"
        }),
    });
    let file_url = format!("file://{}", page.display());
    hub.with_state(|state| {
        assert_eq!(state.tabs[&TabKey::new("companion", "V1")].url, file_url);
    });
    // the flags it cannot have are named, and nothing fails for them
    let both = show(
        &hub,
        &page,
        json!({ "page_api": true, "allow_network": true, "watch": true }),
    );
    assert_eq!(both.text, "showing index.html (network blocked); loads once; show it again after changes (page_api is off for pages on a remote browser; ignored) (allow_network is off for pages on a remote browser; ignored)");
    let op = mac.state.lock().unwrap().ops.last().unwrap().clone();
    assert_eq!(op.args["token"], token, "one token for the directory");
    for absent in [
        "page_api",
        "allow_network",
        "watch",
        "file",
        "root",
        "opener",
    ] {
        assert!(op.args.get(absent).is_none(), "{absent}");
    }
}

#[test]
fn a_token_of_an_older_link_reads_nothing() {
    let (hub, _shared, mac) = linked("link-show-epoch");
    let site = site("show-epoch");
    let page = site.0.join("index.html");
    show(&hub, &page, json!({}));
    let old = mac.state.lock().unwrap().ops.last().unwrap().args["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(pull(&mac, &old, "index.html").status, 200);
    mac.close(LinkEnd::Lost("test".into()));
    let back = FakeMac::open(&hub, "mac-a", "mac").expect("back");
    back.up("i1", &["V1"]);
    wait_for("host_ok", || !back.state.lock().unwrap().leases.is_empty());
    assert_eq!(pull(&back, &old, "index.html").status, 403);
    show(&hub, &page, json!({}));
    let new = back.state.lock().unwrap().ops.last().unwrap().args["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new, old);
    assert_eq!(pull(&back, &new, "index.html").status, 200);
    // the old page's URL still names the file
    back.say(&HelperFrame::Ev {
        event: json!({
            "event": "tab", "target": "V1", "kind": "navigated",
            "url": format!("herdr-file://{old}/index.html"), "initiator": "other"
        }),
    });
    hub.with_state(|state| {
        assert_eq!(
            state.tabs[&TabKey::new("companion", "V1")].url,
            format!("file://{}", page.display())
        );
    });
}

fn screenshot(
    hub: &BrowserHub,
    out: Option<&std::path::Path>,
) -> Result<BrowserRunResult, BrowserError> {
    let mut op = json!({ "op": "screenshot", "format": "png" });
    if let Some(out) = out {
        op["out"] = json!(out.display().to_string());
    }
    run(hub, serde_json::from_value(op).unwrap())
}

/// Every file of a directory, also the hidden ones.
fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn fork_smoke_a_linked_screenshot_lands_here_whole() {
    let (hub, _shared, mac) = linked("link-shot");
    open(&hub).expect("open");
    let taken = screenshot(&hub, None).expect("taken");
    let path = std::path::PathBuf::from(taken.data["path"].as_str().unwrap());
    let inline = std::path::PathBuf::from(taken.data["inline_path"].as_str().unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"the full picture");
    assert_eq!(std::fs::read(&inline).unwrap(), b"tiny");
    assert_eq!(taken.image_path.as_deref(), path.to_str());
    let shots = path.parent().unwrap();
    assert!(
        files_in(shots).iter().all(|name| !name.starts_with('.')),
        "{:?}",
        files_in(shots)
    );
    // the Mac got no path of this machine
    let op = mac.state.lock().unwrap().ops.last().unwrap().clone();
    assert!(op.args.get("path").is_none() && op.args.get("inline_path").is_none());
    // --out
    let site = site("shot-out");
    let out = site.0.join("mine.png");
    let taken = screenshot(&hub, Some(&out)).expect("taken");
    assert_eq!(taken.data["path"], out.display().to_string());
    assert_eq!(std::fs::read(&out).unwrap(), b"the full picture");
    assert_eq!(files_in(&site.0), ["app.js", "index.html", "mine.png"]);
    // no picture: nothing saved, and it says so
    mac.state.lock().unwrap().picture = None;
    let err = screenshot(&hub, Some(&site.0.join("none.png"))).unwrap_err();
    assert_eq!(err.code, "host_failed");
    assert_eq!(
        err.message,
        "the screenshot did not arrive from mac; nothing was saved"
    );
    assert_eq!(files_in(&site.0), ["app.js", "index.html", "mine.png"]);
}

#[test]
fn a_picture_cut_off_leaves_nothing() {
    let (hub, _shared, mac) = linked("link-shot-cut");
    open(&hub).expect("open");
    mac.state.lock().unwrap().picture_cut = true;
    let site = site("shot-cut");
    let out = site.0.join("cut.png");
    let pending = {
        let hub = hub.clone();
        let out = out.clone();
        std::thread::spawn(move || screenshot(&hub, Some(&out)))
    };
    wait_for("its first piece", || files_in(&site.0).len() == 3);
    assert!(files_in(&site.0)
        .iter()
        .any(|name| name.starts_with(".cut.png.part-")));
    mac.close(LinkEnd::Lost("test".into()));
    assert_eq!(pending.join().unwrap().unwrap_err().code, "host_link_lost");
    assert_eq!(files_in(&site.0), ["app.js", "index.html"]);
}

// ---- the companion's calls (SV4)

#[test]
fn a_call_over_the_link_carries_its_own_lease_and_no_path_of_the_mac() {
    let (hub, _shared, mac) = linked("link-api");
    let lease = mac.state.lock().unwrap().leases[0].clone();
    let params = hub
        .api_params(
            &mac.link,
            "browser.host_state",
            json!({ "lease_id": "L-forged" }),
        )
        .unwrap();
    assert_eq!(params["lease_id"], lease);
    assert_eq!(
        hub.api_params(&mac.link, "browser.host_state", json!(null))
            .unwrap()["lease_id"],
        lease
    );
    assert_eq!(
        hub.api_params(&mac.link, "browser.host_state", json!([1]))
            .unwrap_err()
            .code,
        "invalid_request"
    );
    // a page of a shown file is its file here again; what the Mac says of
    // paths, page API and openers is dropped
    let site = site("api");
    let page = site.0.join("index.html");
    show(&hub, &page, json!({}));
    let token = mac.state.lock().unwrap().ops.last().unwrap().args["token"]
        .as_str()
        .unwrap()
        .to_string();
    let synced = hub
        .api_params(
            &mac.link,
            "browser.host_sync",
            json!({ "seq": 1, "groups": [{ "tab_id": "w1:t1", "entries": [
                { "kind": "html_view", "url": format!("herdr-file://{token}/index.html"),
                  "file": "/etc/passwd", "page_api": true, "opener": "o", "serve": token },
                { "kind": "html_view", "url": format!("herdr-file://{}/x.html", "q".repeat(24)),
                  "file": "/etc/passwd" },
                { "kind": "web", "url": "https://a.test/", "file": "/etc/passwd" },
            ] }] }),
        )
        .unwrap();
    let entries = &synced["groups"][0]["entries"];
    assert_eq!(entries[0]["file"], page.display().to_string());
    assert_eq!(entries[0]["url"], format!("file://{}", page.display()));
    for absent in ["page_api", "opener", "serve"] {
        assert!(entries[0].get(absent).is_none(), "{absent}");
    }
    assert_eq!(entries[1]["url"], "about:blank");
    assert!(entries[1].get("file").is_none());
    assert_eq!(entries[2]["url"], "https://a.test/");
    assert!(entries[2].get("file").is_none());
    // and the other way: the Mac gets the page, never the path
    let state = hub.api_result(
        &mac.link,
        "browser.host_state",
        json!({ "type": "browser_host_state", "state": { "tabs": [{ "tab_id": "w1:t1", "entries": [
            { "kind": "html_view", "url": format!("file://{}", page.display()),
              "file": page.display().to_string(), "page_api": true, "opener": "o" },
            { "kind": "html_view", "url": "file:///gone.html", "file": "/gone/gone.html" },
            { "kind": "web", "url": "https://a.test/" },
        ] }] } }),
    );
    let entries = &state["state"]["tabs"][0]["entries"];
    assert_eq!(
        entries[0]["url"],
        format!("herdr-file://{token}/index.html")
    );
    assert_eq!(entries[0]["serve"], token, "the same token as its url");
    for absent in ["file", "page_api", "opener"] {
        assert!(entries[0].get(absent).is_none(), "{absent}");
    }
    assert_eq!(entries[1]["url"], "about:blank");
    assert!(entries[1].get("file").is_none() && entries[1].get("serve").is_none());
    assert_eq!(entries[2]["url"], "https://a.test/");
    // a send of such a page names the file
    let sent = hub
        .api_params(
            &mac.link,
            "browser.send_page",
            json!({ "tab_id": "w1:t1", "url": format!("herdr-file://{token}/index.html") }),
        )
        .unwrap();
    assert_eq!(sent["url"], format!("file://{}", page.display()));
}

#[test]
fn without_its_browser_a_link_has_no_lease_to_lend() {
    let (hub, _shared, mac) = linked("link-api-down");
    mac.say(&HelperFrame::HostDown {
        reason: "gone".into(),
    });
    assert_eq!(
        hub.api_params(&mac.link, "browser.host_state", json!({}))
            .unwrap_err()
            .code,
        "lease_unknown"
    );
}

#[test]
fn browser_get_names_the_mac_linked_reconnecting_and_gone() {
    let (hub, _shared, mac) = linked("link-status");
    let status = hub.get(None).link.expect("linked");
    assert_eq!(
        (status.state.as_str(), status.label.as_str()),
        ("linked", "mac")
    );
    assert_eq!(status.since_unix, mac.link.opened_unix);
    mac.close(LinkEnd::Lost("test".into()));
    let status = hub.get(None).link.expect("reconnecting");
    assert_eq!(status.state, "reconnecting");
    hub.expire_host_lease_for_test();
    hub.supervise();
    let status = hub.get(None).link.expect("gone");
    assert_eq!(
        (status.state.as_str(), status.label.as_str()),
        ("gone", "mac")
    );
    // a server that never had one says nothing
    let (fresh, _home, _shared) = companion_hub("link-status-none", false);
    assert_eq!(fresh.get(None).link, None);
}
