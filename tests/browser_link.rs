//! Fork (remote browser): `browser.link_open` against a real server, with the
//! test as the helper on the user's Mac.
#![cfg(unix)]

pub mod support;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(10);

/// A server of its own: home, config, state and sockets under one directory.
struct Server {
    base: PathBuf,
    socket: PathBuf,
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
}

impl Server {
    fn start(config: &str) -> Self {
        let server = Self::spawn(config);
        // the socket answers before the config is read; this call does not
        assert!(server.request("browser.get", json!({}))["result"].is_object());
        server
    }

    /// The server as soon as its socket answers.
    fn spawn(config: &str) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = PathBuf::from(format!(
            "/tmp/hlink-{}-{}-{nanos}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let runtime = base.join("runtime");
        let socket = runtime.join("herdr.sock");
        for dir in [
            "config/herdr",
            "config/herdr-dev",
            "state",
            "home",
            "runtime",
        ] {
            fs::create_dir_all(base.join(dir)).unwrap();
        }
        for dir in ["herdr", "herdr-dev"] {
            fs::write(
                base.join("config").join(dir).join("config.toml"),
                format!("onboarding = false\n{config}"),
            )
            .unwrap();
        }
        support::register_runtime_dir(&runtime);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
        support::isolate_herdr_test_process(&mut cmd);
        for name in [
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_PANE_ID",
            "HERDR_ENV",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
        ] {
            cmd.env_remove(name);
        }
        cmd.arg("server");
        cmd.env("HOME", base.join("home"));
        cmd.env("XDG_CONFIG_HOME", base.join("config"));
        cmd.env("XDG_STATE_HOME", base.join("state"));
        cmd.env("XDG_RUNTIME_DIR", &runtime);
        cmd.env("HERDR_SOCKET_PATH", &socket);
        cmd.env("SHELL", "/bin/sh");
        let child = pair.slave.spawn_command(cmd).unwrap();
        support::register_spawned_herdr_pid(child.process_id());
        support::wait_for_socket(&socket, WAIT);
        Self {
            base,
            socket,
            child,
            _master: pair.master,
        }
    }

    fn connect(&self) -> Conn {
        Conn {
            stream: UnixStream::connect(&self.socket).unwrap(),
            buf: Vec::new(),
        }
    }

    fn request(&self, method: &str, params: Value) -> Value {
        let mut conn = self.connect();
        conn.send(&json!({ "id": "r", "method": method, "params": params }));
        conn.read()
    }

    /// The connection after a `browser.link_open`, and its answer.
    fn open(&self, params: Value) -> (Conn, Value) {
        let mut conn = self.connect();
        conn.send(&json!({ "id": "link", "method": "browser.link_open", "params": params }));
        let answer = conn.read();
        (conn, answer)
    }

    fn link(&self, params: Value) -> (Conn, Value) {
        let (conn, answer) = self.open(params);
        assert_eq!(answer["id"], "link");
        assert_eq!(answer["result"]["type"], "browser_link_opened", "{answer}");
        (conn, answer["result"]["link"].clone())
    }

    fn refusal(&self, params: Value) -> (String, String) {
        let (mut conn, answer) = self.open(params);
        assert!(conn.at_end(), "a refused open ends its connection");
        let text = |key: &str| answer["error"][key].as_str().unwrap_or("?").to_string();
        (text("code"), text("message"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        support::stop_spawned_herdr(&mut *self.child);
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until && !matches!(self.child.try_wait(), Ok(Some(_))) {
            thread::sleep(Duration::from_millis(20));
        }
        support::unregister_spawned_herdr_pid(pid);
        support::cleanup_test_base(&self.base);
    }
}

struct Conn {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Conn {
    fn send(&mut self, value: &Value) {
        self.send_line(&value.to_string());
    }

    fn send_line(&mut self, line: &str) {
        self.stream.write_all(line.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
    }

    /// The next line; `None` at the end of the stream.
    fn next(&mut self) -> Option<Value> {
        let until = Instant::now() + WAIT;
        self.stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok()?;
        loop {
            if let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=at).collect();
                return Some(serde_json::from_slice(&line).unwrap());
            }
            assert!(Instant::now() < until, "no line and no end");
            let mut bytes = [0u8; 4096];
            match self.stream.read(&mut bytes) {
                Ok(0) => return None,
                Ok(n) => self.buf.extend_from_slice(&bytes[..n]),
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(err) if err.kind() == std::io::ErrorKind::ConnectionReset => return None,
                Err(err) => panic!("{err}"),
            }
        }
    }

    fn read(&mut self) -> Value {
        self.next().expect("the connection ended")
    }

    fn at_end(&mut self) -> bool {
        match self.next() {
            None => true,
            Some(line) => panic!("not the end but {line}"),
        }
    }

    fn ping(&mut self, n: u64) {
        self.send(&json!({ "t": "ping", "n": n }));
        assert_eq!(self.read(), json!({ "t": "pong", "n": n }));
    }

    /// The goodbye and the end after it.
    fn bye(&mut self) -> String {
        let bye = self.read();
        assert_eq!(bye["t"], "bye", "{bye}");
        assert!(self.at_end());
        bye["code"].as_str().unwrap().to_string()
    }
}

fn mac(host_id: &str, label: &str) -> Value {
    json!({
        "link_protocol": 1,
        "host_id": host_id,
        "instance": "i1",
        "label": label,
        "herdr": "0.9.5",
        "features": [],
        "ready": "ready",
    })
}

#[test]
fn a_link_opens_and_the_same_mac_replaces_it() {
    let server = Server::start("");
    let (mut first, info) = server.link(mac("mac-a", "mbp"));
    assert_eq!(info["epoch"], 1);
    assert_eq!(info["link_protocol"], 1);
    assert_eq!(info["features"], json!([]));
    assert_eq!(
        (&info["ping_ms"], &info["ttl_ms"]),
        (&json!(3000), &json!(15000))
    );
    assert!(!info["link_id"].as_str().unwrap().is_empty());
    first.ping(1);
    // a newer helper's frames are nothing to this server
    first.send(&json!({ "t": "from_the_future", "x": 1 }));
    first.send(&json!({ "t": "host_down", "reason": "test", "x": 1 }));
    first.ping(2);

    let (mut second, again) = server.link(mac("mac-a", "mbp"));
    assert_eq!(again["epoch"], 2);
    assert_eq!(again["boot_id"], info["boot_id"]);
    assert_ne!(again["link_id"], info["link_id"]);
    assert_eq!(first.bye(), "replaced");
    // the first connection is gone for a while: the second stays the link
    thread::sleep(Duration::from_millis(600));
    second.ping(1);
    let (code, _) = server.refusal(mac("mac-b", "other"));
    assert_eq!(code, "link_in_use");

    // and the server answers everyone else meanwhile
    assert_eq!(server.request("ping", json!({}))["result"]["protocol"], 25);
}

#[test]
fn another_mac_waits_until_the_first_one_quits() {
    let server = Server::start("");
    let (mut first, _) = server.link(mac("mac-a", "mbp"));
    let (code, message) = server.refusal(mac("mac-b", "other"));
    assert_eq!(code, "link_in_use");
    let rest = message
        .strip_prefix("the browser of ")
        .and_then(|rest| rest.split_once(" is on mbp (since "))
        .and_then(|(_, rest)| rest.strip_suffix("); quit herdr+ there"))
        .unwrap_or_else(|| panic!("{message}"));
    assert!(rest.len() == 5 && &rest[2..3] == ":", "{message}");
    first.ping(1);
    first.send(&json!({ "t": "bye", "code": "quit", "message": "" }));
    assert!(first.at_end());
    let (mut other, info) = server.link(mac("mac-b", "other"));
    assert_eq!(info["epoch"], 2);
    other.ping(1);
    // a connection that just ends frees it too
    drop(other);
    let until = Instant::now() + WAIT;
    loop {
        let (_conn, answer) = server.open(mac("mac-a", "mbp"));
        if answer["result"]["link"]["epoch"] == 3 {
            break;
        }
        assert!(Instant::now() < until, "{answer}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn lines_that_are_no_frames_drop_the_link() {
    let server = Server::start("");
    for bad in [
        "hello",
        "{}",
        r#"{"t":"ping","n":-1}"#,
        r#"{"t":"host_up","instance":7}"#,
        r#"{"t":"blob","op":1,"name":"full","seq":1,"data":"","last":true,"size":0}"#,
    ] {
        let (mut link, _) = server.link(mac("mac-a", "mbp"));
        link.send_line(bad);
        assert_eq!(link.bye(), "protocol", "{bad}");
    }
    let (mut link, _) = server.link(mac("mac-a", "mbp"));
    let mut writer = link.stream.try_clone().unwrap();
    let sender = thread::spawn(move || {
        // the server closes before it is all written
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..20 {
            if writer.write_all(&chunk).is_err() {
                return;
            }
        }
    });
    let mut last = None;
    while let Some(line) = link.next() {
        last = Some(line);
    }
    // the goodbye can be lost with the reset of a line the server did not read
    if let Some(line) = last {
        assert_eq!(line["code"], "protocol");
    }
    sender.join().unwrap();
    let (mut link, info) = server.link(mac("mac-a", "mbp"));
    assert_eq!(info["epoch"], 7);
    link.ping(1);
}

#[test]
fn nothing_is_served_before_there_is_a_browser() {
    let server = Server::start("");
    let (mut link, _) = server.link(mac("mac-a", "mbp"));
    link.send(
        &json!({ "t": "file", "id": 9, "token": "a".repeat(24), "path": "x.html", "offset": 0 }),
    );
    // a token this server never gave
    let reply = link.read();
    assert_eq!(
        (&reply["t"], &reply["id"], &reply["status"]),
        (&json!("file_re"), &json!(9), &json!(403))
    );
    link.send(&json!({ "t": "api", "id": 4, "method": "pane.list", "params": {} }));
    let reply = link.read();
    assert_eq!(
        (&reply["t"], &reply["id"], &reply["ok"]),
        (&json!("api_re"), &json!(4), &json!(false))
    );
    link.send(&json!({ "t": "blob", "op": 1, "name": "inline", "seq": 0, "data": "AAAA", "last": true, "size": 3 }));
    assert_eq!(
        link.read(),
        json!({ "t": "blob_ack", "op": 1, "name": "inline", "seq": 0 })
    );
    link.send(&json!({ "t": "blob", "op": 2, "name": "Thumb-1", "seq": 0, "data": "", "last": true, "size": 0 }));
    assert_eq!(
        link.read(),
        json!({ "t": "blob_ack", "op": 2, "name": "Thumb-1", "seq": 0 })
    );
    link.ping(1);
}

#[test]
fn an_open_is_refused_by_its_parameters() {
    let server = Server::start("");
    let with = |key: &str, value: Value| {
        let mut params = mac("mac-a", "mbp");
        params[key] = value;
        server.refusal(params).0
    };
    assert_eq!(with("link_protocol", json!(2)), "link_protocol_unsupported");
    assert_eq!(with("host_id", json!(" ")), "invalid_request");
    assert_eq!(with("instance", json!("")), "invalid_request");
    assert_eq!(with("ready", json!(null)), "invalid_request");
    // a newer helper's value is one to this server
    let mut params = mac("mac-a", "mbp");
    params["ready"] = json!("updating");
    let (_link, info) = server.link(params);
    assert_eq!(info["epoch"], 1);

    let off = Server::start("[browser]\nenabled = false\n");
    assert_eq!(off.refusal(mac("mac-a", "mbp")).0, "browser_disabled");
}

#[test]
fn a_link_taken_before_the_config_arrives_ends_when_it_does() {
    for config in [
        "[browser]\nenabled = false\n",
        "[browser]\nlink = \"off\"\n",
    ] {
        let server = Server::spawn(config);
        // at once: before or after the server read its config
        let (mut conn, answer) = server.open(mac("mac-a", "mbp"));
        if answer["result"]["type"] == "browser_link_opened" {
            assert_eq!(conn.bye(), "disabled", "{config}");
        } else {
            assert!(conn.at_end());
            let code = answer["error"]["code"].as_str().unwrap().to_string();
            assert!(
                ["browser_disabled", "link_disabled"].contains(&code.as_str()),
                "{answer}"
            );
        }
        let expected = if config.contains("enabled") {
            "browser_disabled"
        } else {
            "link_disabled"
        };
        assert_eq!(server.refusal(mac("mac-a", "mbp")).0, expected);
    }
}

#[test]
fn the_local_companion_still_registers_over_loopback_only() {
    let server = Server::start("");
    let answer = server.request(
        "browser.host_register",
        json!({
            "instance": "i", "nonce": "n", "protocol": 1,
            "cdp": { "host": "192.168.1.4", "port": 9222 },
        }),
    );
    assert_eq!(answer["error"]["code"], "invalid_request", "{answer}");
}

#[test]
fn a_server_that_stops_says_goodbye() {
    let server = Server::start("");
    let (mut link, _) = server.link(mac("mac-a", "mbp"));
    link.ping(1);
    server.request("server.stop", json!({}));
    let mut last = None;
    let started = Instant::now();
    while let Some(line) = link.next() {
        last = Some(line);
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    // the process may be gone before its goodbye
    if let Some(line) = last {
        assert_eq!(
            (&line["t"], &line["code"]),
            (&json!("bye"), &json!("stopping"))
        );
    }
}

/// The companion's calls over the link: only its few methods, always with
/// the link's own lease, and its events as frames.
#[test]
fn the_companions_calls_run_here_with_the_links_own_lease() {
    let server = Server::start("");
    let (mut link, _) = server.link(mac("mac-a", "mbp"));
    let mut next = 0;
    let mut call = |link: &mut Conn, method: &str, params: Value| {
        next += 1;
        link.send(&json!({ "t": "api", "id": next, "method": method, "params": params }));
        loop {
            let frame = link.read();
            if frame["t"] == "api_re" && frame["id"] == next {
                return frame;
            }
        }
    };
    // no browser up there yet: no lease
    let early = call(&mut link, "browser.host_state", json!({}));
    assert_eq!(early["error"]["code"], "lease_unknown", "{early}");
    link.send(&json!({ "t": "host_up", "instance": "i1", "version": "0.2.13", "tabs": [] }));
    let ok = link.read();
    assert_eq!(ok["t"], "host_ok", "{ok}");
    // never relayed
    for method in [
        "browser.page_message",
        "pane.list",
        "browser.host_register",
        "server.stop",
    ] {
        let refused = call(&mut link, method, json!({}));
        assert_eq!(refused["ok"], false, "{method}");
        assert_eq!(
            refused["error"]["message"],
            format!("{method} is not available over the browser link")
        );
    }
    // a lease the Mac makes up is replaced by the link's
    let state = call(
        &mut link,
        "browser.host_state",
        json!({ "lease_id": "L-forged" }),
    );
    assert_eq!(state["ok"], true, "{state}");
    assert_eq!(state["result"]["type"], "browser_host_state");
    let report = call(
        &mut link,
        "browser.host_report",
        json!({ "lease_id": "x", "events": [] }),
    );
    assert_eq!(
        report["result"]["type"], "browser_host_reported",
        "{report}"
    );
    // its events, and the rest of its methods
    let subscribed = call(
        &mut link,
        "events.subscribe",
        json!({ "subscriptions": [{ "type": "tab.created" }] }),
    );
    assert_eq!(
        subscribed["result"]["type"], "subscription_started",
        "{subscribed}"
    );
    let not_its = call(
        &mut link,
        "events.subscribe",
        json!({ "subscriptions": [{ "type": "pane.output_matched" }] }),
    );
    assert_eq!(not_its["error"]["code"], "invalid_request");
    let dir = server.base.join("home");
    let created = server.request(
        "workspace.create",
        json!({ "cwd": dir.display().to_string(), "focus": true }),
    );
    let workspace = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    server.request(
        "tab.create",
        json!({ "workspace_id": workspace, "focus": false }),
    );
    let event = loop {
        let frame = link.read();
        if frame["t"] == "event" {
            break frame;
        }
    };
    assert_eq!(event["event"]["event"], "tab_created", "{event}");
    let tab_id = event["event"]["data"]["tab_id"]
        .as_str()
        .or_else(|| event["event"]["data"]["tab"]["tab_id"].as_str())
        .unwrap_or_else(|| panic!("{event}"))
        .to_string();
    let focused = call(&mut link, "tab.focus", json!({ "tab_id": tab_id }));
    assert_eq!(focused["ok"], true, "{focused}");
    // a path the Mac names is never this machine's
    let synced = call(
        &mut link,
        "browser.host_sync",
        json!({ "seq": 1, "groups": [{ "tab_id": tab_id, "entries": [{
            "view_id": "v1", "target": "T1", "kind": "html_view",
            "url": format!("herdr-file://{}/x.html", "q".repeat(24)),
            "file": "/etc/passwd", "page_api": true, "opener": "o"
        }] }] }),
    );
    assert_eq!(synced["ok"], true, "{synced}");
    let state = call(
        &mut link,
        "browser.host_state",
        json!({ "tab_ids": [tab_id] }),
    );
    let entry = &state["result"]["state"]["tabs"][0]["entries"][0];
    assert_eq!(entry["url"], "about:blank", "{state}");
    for absent in ["file", "page_api", "opener", "serve"] {
        assert!(entry.get(absent).is_none(), "{absent}: {state}");
    }
    // what browser.get says about it
    let info = server.request("browser.get", json!({}));
    let linked = &info["result"]["browser"]["link"];
    assert_eq!(
        (&linked["state"], &linked["label"]),
        (&json!("linked"), &json!("mbp")),
        "{info}"
    );
}
