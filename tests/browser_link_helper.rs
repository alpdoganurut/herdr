//! Fork (remote browser), I1: the whole link end to end on one Mac.
//!
//! "B" is a second herdr server in a directory of its own (`[browser] link =
//! "only"`); "A" is the real helper (`herdr browser-link <machine>`) with the
//! real companion (headless, this tree's app files) and the real sidecar.
//! Between them an `ssh` stand-in (`browser_link_helper/ssh_shim.py`) runs
//! B's commands under B's environment and serves the masters and their
//! forwards. The test is A's client on the helper's `.link` socket and an
//! agent in a B pane (`herdr browser …`).
//!
//! It needs node, a built companion app's `node_modules` (Electron and
//! playwright-core: `RB_I1_NODE_MODULES`, used in place for Electron, copied
//! with `cp -Rc` for playwright-core) and runs only when asked:
//! `RB_I1_NODE_MODULES=… cargo nextest run --test browser_link_helper
//! --run-ignored only`. Every process it starts has a hard alarm and ends by
//! its input, its control socket or the server's own stop.
#![cfg(target_os = "macos")]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const SHIM: &str = include_str!("browser_link_helper/ssh_shim.py");
const PROFILE: &str = "0123456789abcdef0123456789abcdef";
const MACHINE: &str = "devhost1";
const APP: &str = if cfg!(debug_assertions) {
    "herdr-dev"
} else {
    "herdr"
};

fn herdr() -> &'static str {
    env!("CARGO_BIN_EXE_herdr")
}

/// `program args…` under a hard alarm of `secs`.
fn alarmed(secs: u32, program: &str) -> Command {
    let mut command = Command::new("/usr/bin/perl");
    command
        .arg("-e")
        .arg(format!("alarm {secs}; exec @ARGV"))
        .arg(program);
    command
}

fn copy(from: &Path, to: &Path) {
    let status = Command::new("/bin/cp")
        .arg("-Rc")
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "cp -Rc {} {}",
        from.display(),
        to.display()
    );
}

fn until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(100));
    }
}

fn request(socket: &Path, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        json!({ "id": "t", "method": method, "params": params })
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or(Value::Null)
}

/// A tiny web server: `/` is a page with a button; every request counted.
fn web(listener: TcpListener, hits: Arc<AtomicUsize>) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            hits.fetch_add(1, Ordering::SeqCst);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                let body = "<!doctype html><title>B page</title><h1 id=t>Served on the remote machine</h1>\
                    <button id=go onclick=\"document.getElementById('t').textContent='clicked on B'\">go</button>\
                    <input id=q onkeydown=\"if (event.key === 'Enter') document.getElementById('t').textContent = 'entered ' + this.value\">";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
}

/// Counts every connection (nothing may reach it).
fn canary(listener: TcpListener, hits: Arc<AtomicUsize>) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            if stream.is_ok() {
                hits.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
}

/// Everything the run made; dropping it stops what still runs (by asking)
/// and removes the directory.
struct Rig {
    root: PathBuf,
    b_socket: PathBuf,
    b_server: Option<Child>,
    helper: Option<Child>,
    client: Option<UnixStream>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        // the reader's copy of the socket goes with it: the helper sees its
        // client leave
        if let Some(client) = self.client.take() {
            let _ = client.shutdown(std::net::Shutdown::Both);
        }
        if let Some(mut helper) = self.helper.take() {
            // it ends 30 s after its last client (its alarm is the limit)
            let deadline = Instant::now() + Duration::from_secs(45);
            while Instant::now() < deadline && matches!(helper.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(200));
            }
            let _ = helper.try_wait();
        }
        if let Some(mut server) = self.b_server.take() {
            let _ = UnixStream::connect(&self.b_socket).map(|mut stream| {
                let _ = writeln!(
                    stream,
                    "{}",
                    json!({ "id": "stop", "method": "server.stop", "params": {} })
                );
            });
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && matches!(server.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(100));
            }
            let _ = server.try_wait();
        }
        if thread::panicking() && std::env::var_os("RB_I1_KEEP").is_some() {
            eprintln!("kept for a look: {}", self.root.display());
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

struct Client {
    stream: UnixStream,
    states: Arc<Mutex<Vec<Value>>>,
}

impl Client {
    fn send(&mut self, message: Value) {
        writeln!(self.stream, "{message}").unwrap();
    }

    fn last(&self) -> Option<Value> {
        self.states.lock().unwrap().last().cloned()
    }

    fn wait_for(&self, what: &str, timeout: Duration, test: impl Fn(&Value) -> bool) -> Value {
        let mut found = None;
        until(what, timeout, || {
            found = self.last().filter(|state| test(state));
            found.is_some()
        });
        found.unwrap()
    }
}

/// `herdr browser <args> --json` as an agent in `pane` on B.
fn b_cli(root: &Path, pane: &str, args: &[&str]) -> (Value, Output) {
    let b = root.join("b");
    let output = alarmed(120, herdr())
        .arg("browser")
        .args(args)
        .args(["--pane", pane, "--json"])
        .env_clear()
        .env("HOME", b.join("home"))
        .env("XDG_CONFIG_HOME", b.join("config"))
        .env("XDG_STATE_HOME", b.join("state"))
        .env("XDG_RUNTIME_DIR", b.join("run"))
        .env(
            "HERDR_SOCKET_PATH",
            b.join("config").join(APP).join("herdr.sock"),
        )
        .env("HERDR_PANE_ID", pane)
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    (value, output)
}

fn ok(what: &str, (value, output): (Value, Output)) -> Value {
    assert!(
        output.status.success(),
        "{what}: {value}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    value
}

fn files_named(dir: &Path, suffix: &str, skip: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == skip) {
            continue;
        }
        if path.is_dir() && !path.is_symlink() {
            files_named(&path, suffix, skip, found);
        } else if path.to_string_lossy().ends_with(suffix) {
            found.push(path);
        }
    }
}

#[test]
#[ignore = "starts a hidden Electron companion, node and two herdr processes; needs RB_I1_NODE_MODULES"]
fn the_remote_machines_agents_browse_on_this_mac_through_the_helper() {
    let modules = PathBuf::from(std::env::var("RB_I1_NODE_MODULES").expect("RB_I1_NODE_MODULES"));
    let node = Command::new("/bin/sh")
        .args(["-c", "command -v node"])
        .output()
        .unwrap();
    let node = String::from_utf8_lossy(&node.stdout).trim().to_owned();
    assert!(!node.is_empty(), "node on PATH");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    // short: socket paths have to fit
    let root = PathBuf::from(format!("/tmp/hri-{}-{nanos:x}", std::process::id()));
    let (a, b) = (root.join("a"), root.join("b"));
    let mut rig = Rig {
        root: root.clone(),
        // the session `default`'s socket: where the remote API bridge looks
        b_socket: b.join("config").join(APP).join("herdr.sock"),
        b_server: None,
        helper: None,
        client: None,
    };
    for dir in [
        "bin",
        "s",
        "a/home",
        "a/state",
        "a/run",
        "b/home/.local/bin",
        "b/state",
        "b/run",
        "b/bin",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::set_permissions(root.join("s"), fs::Permissions::from_mode(0o700)).unwrap();
    for (side, config) in [
        (&b, "onboarding = false\n[browser]\nlink = \"only\"\n"),
        (
            &a,
            "onboarding = false\n[browser]\nremote_machines = true\negress = \"split\"\n",
        ),
    ] {
        fs::create_dir_all(side.join("config").join(APP)).unwrap();
        fs::write(side.join("config").join(APP).join("config.toml"), config).unwrap();
    }

    // B: the remote machine's server and its herdr on PATH
    for bin in [b.join("bin/herdr"), b.join("home/.local/bin/herdr")] {
        std::os::unix::fs::symlink(herdr(), bin).unwrap();
    }
    let b_env = json!({
        "HOME": b.join("home"),
        "XDG_CONFIG_HOME": b.join("config"),
        "XDG_STATE_HOME": b.join("state"),
        "XDG_RUNTIME_DIR": b.join("run"),
        "HERDR_SOCKET_PATH": rig.b_socket,
        "PATH": format!("{}:/usr/bin:/bin", b.join("bin").display()),
        "SHELL": "/bin/sh",
    });
    fs::write(root.join("b-env.json"), b_env.to_string()).unwrap();
    let server = alarmed(600, herdr())
        .arg("server")
        .env_clear()
        .env("HOME", b.join("home"))
        .env("XDG_CONFIG_HOME", b.join("config"))
        .env("XDG_STATE_HOME", b.join("state"))
        .env("XDG_RUNTIME_DIR", b.join("run"))
        .env("HERDR_SOCKET_PATH", &rig.b_socket)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            fs::File::create(root.join("b-server.log")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let b_pid = server.id();
    rig.b_server = Some(server);
    until("B's socket", Duration::from_secs(20), || {
        UnixStream::connect(&rig.b_socket).is_ok()
    });

    // the pages: B's localhost:P is the web server, this Mac's is a canary
    let canary_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = canary_listener.local_addr().unwrap().port();
    let canary_hits = Arc::new(AtomicUsize::new(0));
    canary(canary_listener, Arc::clone(&canary_hits));
    let web_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let web_port = web_listener.local_addr().unwrap().port();
    let web_hits = Arc::new(AtomicUsize::new(0));
    web(web_listener, Arc::clone(&web_hits));
    fs::write(
        root.join("dialmap.json"),
        json!({ format!("localhost:{port}"): web_port }).to_string(),
    )
    .unwrap();
    fs::write(
        root.join("bin/ssh"),
        format!("#!/usr/bin/python3 -I\n{SHIM}"),
    )
    .unwrap();
    fs::set_permissions(root.join("bin/ssh"), fs::Permissions::from_mode(0o700)).unwrap();

    // A: the catalog, the companion app and the sidecar
    let state = a.join("state").join(APP);
    fs::create_dir_all(state.join("client")).unwrap();
    fs::write(
        state.join("client/endpoints.json"),
        json!({ "version": 1, "ssh": [{ "id": PROFILE, "label": MACHINE, "target": MACHINE,
                 "session": "default", "enabled": true }] })
        .to_string(),
    )
    .unwrap();
    let browser_home = state.join("browser");
    let companion_home = browser_home.join("companion-app");
    let sha = "e1".repeat(32);
    let app_dir = companion_home.join("app").join(&sha[..12]);
    fs::create_dir_all(&app_dir).unwrap();
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/companion");
    for part in ["package.json", "src", "assets"] {
        copy(&app.join(part), &app_dir.join(part));
    }
    let version: Value =
        serde_json::from_slice(&fs::read(app.join("package.json")).unwrap()).unwrap();
    let electron: Value =
        serde_json::from_slice(&fs::read(modules.join("electron/package.json")).unwrap()).unwrap();
    // Electron cloned into the rig (cp -Rc), never linked
    fs::create_dir_all(root.join("electron")).unwrap();
    copy(
        &modules.join("electron/dist/Electron.app"),
        &root.join("electron/Electron.app"),
    );
    fs::write(
        companion_home.join("install.json"),
        json!({ "version": 1, "app": version["version"], "app_sha256": sha,
                "electron": electron["version"],
                "bundle": root.join("electron/Electron.app") })
        .to_string(),
    )
    .unwrap();
    let host = browser_home.join("host");
    fs::create_dir_all(host.join("node_modules")).unwrap();
    copy(
        &modules.join("playwright-core"),
        &host.join("node_modules/playwright-core"),
    );
    fs::write(
        host.join("runtime.json"),
        json!({ "version": 1, "node": node, "assets_sha256": "" }).to_string(),
    )
    .unwrap();

    // the helper, as a client starts it
    let node_dir = Path::new(&node).parent().unwrap().display().to_string();
    let helper = alarmed(300, herdr())
        .args(["browser-link", PROFILE])
        .env_clear()
        .env("HOME", a.join("home"))
        .env("XDG_CONFIG_HOME", a.join("config"))
        .env("XDG_STATE_HOME", a.join("state"))
        .env("XDG_RUNTIME_DIR", a.join("run"))
        .env("HERDR_BROWSER_LINK_SOCKET_DIR", root.join("s"))
        .env("HERDR_COMPANION_HEADLESS", "1")
        .env("HERDR_LOG", "herdr=debug")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin:{node_dir}", root.join("bin").display()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            fs::File::create(root.join("helper.log")).unwrap(),
        ))
        .spawn()
        .unwrap();
    rig.helper = Some(helper);
    let link = {
        let mut found = None;
        until("the helper's .link socket", Duration::from_secs(20), || {
            found = fs::read_dir(root.join("s"))
                .unwrap()
                .flatten()
                .map(|entry| entry.path())
                .find(|path| path.extension().is_some_and(|ext| ext == "link"));
            found.is_some()
        });
        found.unwrap()
    };
    let stream = UnixStream::connect(&link).unwrap();
    rig.client = Some(stream.try_clone().unwrap());
    let states: Arc<Mutex<Vec<Value>>> = Arc::default();
    {
        let (reader, states) = (stream.try_clone().unwrap(), Arc::clone(&states));
        thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { return };
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value["t"] == "state" {
                        states.lock().unwrap().push(value);
                    }
                }
            }
        });
    }
    let mut client = Client { stream, states };
    client.send(json!({ "t": "hello", "proto": 1, "herdr": "i1",
        "machine": { "id": PROFILE, "label": MACHINE, "target": MACHINE, "session": "default" },
        "cycle_keys": [] }));
    client.wait_for("linked", Duration::from_secs(30), |state| {
        state["link"] == "linked"
    });

    // an agent in a B pane
    let workspace = request(
        &rig.b_socket,
        "workspace.create",
        json!({ "cwd": b.join("home"), "focus": true }),
    );
    let pane = workspace["result"]["root_pane"]["pane_id"]
        .as_str()
        .or_else(|| workspace["result"]["workspace"]["root_pane"]["pane_id"].as_str())
        .unwrap_or_else(|| panic!("{workspace}"))
        .to_owned();
    let tab = workspace["result"]["root_pane"]["tab_id"]
        .as_str()
        .or_else(|| workspace["result"]["workspace"]["focused_tab_id"].as_str())
        .unwrap_or_default()
        .to_owned();

    // open, read, click, screenshot: the companion on this Mac does them
    let url = format!("http://localhost:{port}/");
    let started = Instant::now();
    let opened = ok(
        "open",
        b_cli(&root, &pane, &["open", &url, "--timeout", "90000"]),
    );
    eprintln!(
        "first open in {} ms: {opened}",
        started.elapsed().as_millis()
    );
    let state = client.last().unwrap();
    assert!(
        matches!(state["companion"]["state"].as_str(), Some("running")),
        "{state}"
    );
    let read = ok("read", b_cli(&root, &pane, &["read"]));
    assert!(
        read.to_string().contains("Served on the remote machine"),
        "{read}"
    );
    ok(
        "click",
        // a hidden window draws no frames, so Playwright's click never
        // sees the button stable there (as for any hidden companion page);
        // the page's own click runs through the gate as an `eval`
        b_cli(
            &root,
            &pane,
            &["eval", "document.getElementById('go').click()"],
        ),
    );
    let read = ok("read after the click", b_cli(&root, &pane, &["read"]));
    assert!(read.to_string().contains("clicked on B"), "{read}");
    // the act family through the gate and the page's input queue
    ok(
        "fill",
        b_cli(&root, &pane, &["fill", "--selector", "#q", "hello"]),
    );
    ok(
        "press",
        b_cli(&root, &pane, &["press", "--selector", "#q", "Enter"]),
    );
    let typed = ok(
        "the field after the keys",
        b_cli(
            &root,
            &pane,
            &["eval", "document.getElementById('q').value + '|' + document.getElementById('t').textContent"],
        ),
    );
    // the fill landed; the key went through the gate and the page's queue,
    // but a hidden window delivers no key events (as for any hidden
    // companion page), so the page's Enter handler is not asserted
    assert_eq!(
        typed.pointer("/result/result/data/value"),
        Some(&json!("hello|clicked on B")),
        "{typed}"
    );
    let shot = ok("screenshot", b_cli(&root, &pane, &["screenshot"]));
    let path = shot
        .pointer("/result/result/data/path")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{shot}"))
        .to_owned();
    assert!(path.starts_with(&b.display().to_string()), "{path}");
    let bytes = fs::read(&path).unwrap();
    assert!(
        bytes.starts_with(b"\xff\xd8\xff") || bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "a picture: {path}"
    );
    let mut pictures = Vec::new();
    // the machine's home (its profile is the browser's own), this Mac's home
    // and runtime directory, the sockets
    for dir in [
        companion_home.join("machines"),
        a.join("home"),
        a.join("run"),
        root.join("s"),
    ] {
        for suffix in [".png", ".jpg", ".jpeg"] {
            files_named(&dir, suffix, "profile", &mut pictures);
        }
    }
    assert!(
        pictures.is_empty(),
        "nothing is left on this Mac: {pictures:?}"
    );

    // a file of B's shows here
    let page = b.join("home/shown.html");
    fs::write(
        &page,
        "<!doctype html><title>shown</title><h1>Shown from the remote machine</h1>",
    )
    .unwrap();
    ok(
        "show",
        b_cli(&root, &pane, &["show", page.to_str().unwrap()]),
    );
    let read = ok("read the shown file", b_cli(&root, &pane, &["read"]));
    assert!(
        read.to_string().contains("Shown from the remote machine"),
        "{read}"
    );

    // localhost meant B's, never this Mac's
    assert!(web_hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        canary_hits.load(Ordering::SeqCst),
        0,
        "this Mac's localhost was dialed"
    );
    let dials = fs::read_to_string(root.join("dials.log")).unwrap_or_default();
    assert!(dials.contains(&format!("localhost:{port}")), "{dials}");

    // nothing listens on TCP on B
    let listening = Command::new("/usr/sbin/lsof")
        .args([
            "-a",
            "-nP",
            "-iTCP",
            "-sTCP:LISTEN",
            "-p",
            &b_pid.to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&listening.stdout).trim().is_empty(),
        "{}",
        String::from_utf8_lossy(&listening.stdout)
    );
    // the same look at this test's own process finds its two listeners
    let own = Command::new("/usr/sbin/lsof")
        .args(["-a", "-nP", "-iTCP", "-sTCP:LISTEN", "-p"])
        .arg(std::process::id().to_string())
        .output()
        .unwrap();
    let own = String::from_utf8_lossy(&own.stdout);
    assert!(
        own.contains(&format!(":{port} ")) && own.contains(&format!(":{web_port} ")),
        "{own}"
    );

    // the dock: shown while this client shows the machine, hidden after a switch
    let spec = |shown: bool| {
        json!({ "id": "pl-i1", "kind": "side", "tab_id": tab,
                "area": { "x": 40, "y": 1, "width": 40, "height": 20 },
                "grid": { "cols": 80, "rows": 24 }, "cell_px": { "width": 10, "height": 20 },
                "extent_px": { "width": 800, "height": 480 }, "pids": [], "shown": shown, "nudge": 0 })
    };
    let report = |state: &Value| {
        state["reports"]
            .as_array()
            .and_then(|reports| reports.iter().find(|r| r["id"] == "pl-i1").cloned())
    };
    client.send(json!({ "t": "placement", "spec": spec(true), "active": true, "focused": true }));
    let shown = client.wait_for(
        "a report on the placement",
        Duration::from_secs(15),
        |state| report(state).is_some(),
    );
    eprintln!("shown: {}", report(&shown).unwrap());
    client.send(json!({ "t": "placement", "spec": spec(true), "active": false, "focused": true }));
    client.wait_for(
        "hidden after the switch",
        Duration::from_secs(15),
        |state| {
            state["reports"].as_array().is_some_and(|reports| {
                reports
                    .iter()
                    .any(|r| r["id"] == "pl-i1" && r["state"] == "hidden")
            })
        },
    );
    client.send(json!({ "t": "placement", "spec": spec(true), "active": true, "focused": true }));
    thread::sleep(Duration::from_millis(1500));
    eprintln!(
        "shown again: {}",
        client
            .last()
            .and_then(|state| report(&state))
            .unwrap_or(Value::Null)
    );

    // a drop mid-op: the op says so at once, the same tab is there after
    let tabs_before = ok("tabs", b_cli(&root, &pane, &["tabs"]));
    let (pane2, root2) = (pane.clone(), root.clone());
    let slow = thread::spawn(move || {
        b_cli(
            &root2,
            &pane2,
            &[
                "eval",
                "new Promise((done) => setTimeout(() => done(1), 15000))",
            ],
        )
    });
    thread::sleep(Duration::from_millis(2000));
    let dropped_at = Instant::now();
    fs::write(root.join("drop"), "").unwrap();
    let (value, output) = slow.join().unwrap();
    let answered_in = dropped_at.elapsed();
    let text = format!("{value} {}", String::from_utf8_lossy(&output.stderr));
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains("host_link_lost") || text.contains("dropped while"),
        "{text}"
    );
    assert!(answered_in < Duration::from_secs(5), "{answered_in:?}");
    client.wait_for("reconnecting", Duration::from_secs(20), |state| {
        state["link"] != "linked"
    });
    fs::remove_file(root.join("drop")).unwrap();
    client.wait_for("linked again", Duration::from_secs(30), |state| {
        state["link"] == "linked"
    });
    let tabs_after = ok("tabs after", b_cli(&root, &pane, &["tabs"]));
    let ids = |tabs: &Value| {
        let mut ids: Vec<String> = tabs
            .to_string()
            .match_indices("companion:t")
            .map(|(at, _)| {
                tabs.to_string()[at..]
                    .chars()
                    .take_while(|c| *c != '"')
                    .collect()
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    };
    assert!(!ids(&tabs_before).is_empty(), "{tabs_before}");
    assert_eq!(
        ids(&tabs_before),
        ids(&tabs_after),
        "{tabs_before}\n{tabs_after}"
    );

    // the end: the client leaves, the helper ends with its companion
    let companion_pid = client
        .last()
        .and_then(|state| state["companion"]["pid"].as_u64());
    drop(client);
    if let Some(stream) = rig.client.take() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    let mut helper = rig.helper.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && matches!(helper.try_wait(), Ok(None)) {
        thread::sleep(Duration::from_millis(200));
    }
    let status = helper.try_wait().unwrap();
    assert!(
        status.is_some(),
        "the helper ended 30 s after its last client"
    );
    if let Some(pid) = companion_pid {
        let alive = Command::new("/bin/ps")
            .args(["-p", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!alive, "the companion quit with its helper");
    }
    let left: Vec<_> = fs::read_dir(root.join("s")).unwrap().flatten().collect();
    assert!(left.is_empty(), "its sockets are gone: {left:?}");
}
