//! The sidecar against a fake `host.mjs` on socket pairs: no node, no
//! processes.

use std::io::{BufReader, Write as _};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;
use crate::browser::link_proto::LinkOp;
use crate::client::browser_link::gate::{Gate, Policy, Verdict};

/// What the fake sidecar does with a request: the lines it writes back.
type Behavior = dyn Fn(&Value) -> Vec<String> + Send + Sync;

fn reply(request: &Value, result: Value) -> String {
    json!({ "id": request["id"], "ok": true, "result": result }).to_string()
}

struct Fake {
    behavior: Box<Behavior>,
    spawns: AtomicUsize,
    requests: Arc<Mutex<Vec<Value>>>,
    kills: Mutex<Vec<u32>>,
    /// Each process's stdout, to end it from outside.
    outs: Mutex<HashMap<u32, UnixStream>>,
    /// Processes that saw their stdin end.
    eofs: Arc<AtomicUsize>,
    /// Pings go unanswered.
    mute: Arc<AtomicBool>,
}

impl Fake {
    fn new(behavior: impl Fn(&Value) -> Option<Vec<String>> + Send + Sync + 'static) -> Arc<Self> {
        let mute = Arc::new(AtomicBool::new(false));
        let quiet = Arc::clone(&mute);
        Arc::new(Self {
            behavior: Box::new(move |request: &Value| {
                if let Some(lines) = behavior(request) {
                    return lines;
                }
                match request["op"].as_str() {
                    Some("hello") => vec![reply(
                        request,
                        json!({ "host_protocol": HOST_PROTOCOL, "playwright_version": "1.63.0" }),
                    )],
                    Some("ping") if quiet.load(Ordering::Relaxed) => Vec::new(),
                    Some("attach") => vec![reply(
                        request,
                        json!({ "tabs": [], "companion": { "state": "host" } }),
                    )],
                    Some("control_quit") => Vec::new(),
                    _ => vec![reply(request, json!({}))],
                }
            }),
            spawns: AtomicUsize::new(0),
            requests: Arc::new(Mutex::new(Vec::new())),
            kills: Mutex::new(Vec::new()),
            outs: Mutex::new(HashMap::new()),
            eofs: Arc::new(AtomicUsize::new(0)),
            mute,
        })
    }

    fn requests(&self, op: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["op"] == op)
            .cloned()
            .collect()
    }

    /// The running process's stdout ends (it crashed).
    fn crash(&self, pid: u32) {
        if let Some(out) = self.outs.lock().unwrap().remove(&pid) {
            let _ = out.shutdown(Shutdown::Both);
        }
    }
}

impl SidecarSpawn for Arc<Fake> {
    fn spawn(&self) -> io::Result<SidecarProcess> {
        let n = self.spawns.fetch_add(1, Ordering::Relaxed) as u32;
        let pid = 9000 + n;
        let (ours, theirs_in) = UnixStream::pair()?;
        let (mut theirs_out, our_in) = UnixStream::pair()?;
        self.outs
            .lock()
            .unwrap()
            .insert(pid, theirs_out.try_clone()?);
        let fake = Arc::clone(self);
        std::thread::spawn(move || {
            let mut input = BufReader::new(theirs_in);
            let mut line = String::new();
            loop {
                line.clear();
                match input.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                fake.requests.lock().unwrap().push(request.clone());
                for out in (fake.behavior)(&request) {
                    if theirs_out
                        .write_all(out.as_bytes())
                        .and_then(|()| theirs_out.write_all(b"\n"))
                        .is_err()
                    {
                        return;
                    }
                }
            }
            fake.eofs.fetch_add(1, Ordering::Relaxed);
        });
        Ok(SidecarProcess {
            pid,
            child: None,
            writer: Box::new(ours),
            reader: Box::new(BufReader::new(our_in)),
        })
    }

    fn kill(&self, pid: u32) {
        self.kills.lock().unwrap().push(pid);
        self.crash(pid);
    }
}

#[derive(Default)]
struct Hooks {
    events: Mutex<Vec<Value>>,
    attached: AtomicUsize,
    lost: Mutex<Vec<String>>,
}

impl SidecarHooks for Hooks {
    fn event(&self, event: Value) {
        self.events.lock().unwrap().push(event);
    }

    fn attached(&self, _answer: &Value) {
        self.attached.fetch_add(1, Ordering::Relaxed);
    }

    fn lost(&self, reason: &str) {
        self.lost.lock().unwrap().push(reason.to_string());
    }
}

fn options() -> SidecarOptions {
    SidecarOptions {
        // the tests tick it themselves
        ping_every: Duration::from_secs(3600),
        ping_timeout: Duration::from_millis(100),
        misses: PING_MISSES,
        line_max: LINE_MAX,
    }
}

fn rig(fake: &Arc<Fake>, options: SidecarOptions) -> (Sidecar, Arc<Hooks>) {
    let hooks = Arc::new(Hooks::default());
    let sidecar = Sidecar::new(Arc::new(Arc::clone(fake)), hooks.clone(), options);
    (sidecar, hooks)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_attach_is_remote_with_the_companions_own_port_and_nonce() {
    let fake = Fake::new(|_| None);
    let (sidecar, hooks) = rig(&fake, options());
    let answer = sidecar.attach_companion(9333, "nonce-1").unwrap();
    assert_eq!(answer["companion"]["state"], "host");
    assert!(sidecar.attached());
    let hello = fake.requests("hello");
    assert_eq!(hello.len(), 1);
    assert!(hello[0].get("profile").is_none());
    let attach = fake.requests("attach");
    assert_eq!(attach.len(), 1);
    assert_eq!(attach[0]["profile"], COMPANION_PROFILE);
    assert_eq!(
        attach[0]["args"],
        json!({ "port": 9333, "kind": "companion", "nonce": "nonce-1", "remote": true })
    );
    assert_eq!(attach[0]["deadline_ms"], ATTACH_TIMEOUT.as_millis() as u64);
    assert_eq!(hooks.attached.load(Ordering::Relaxed), 1);
    // a second attach uses the same sidecar
    sidecar.attach_companion(9333, "nonce-1").unwrap();
    assert_eq!(fake.spawns.load(Ordering::Relaxed), 1);
}

#[test]
fn a_gated_op_goes_out_under_the_companion_profile_with_the_helpers_id() {
    let fake = Fake::new(|request| {
        (request["op"] == "read").then(|| vec![reply(request, json!({ "text": "hi" }))])
    });
    let (sidecar, _) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    let mut gate = Gate::new("mac", Duration::from_secs(1));
    let echo = gate.ping(Duration::from_secs(1));
    let op = LinkOp {
        id: 77,
        echo,
        op: "read".into(),
        target: Some("T1".into()),
        args: Value::Null,
        deadline_ms: 5000,
        activity: None,
    };
    let policy = Policy {
        allow_eval: false,
        allow_password: false,
    };
    let Verdict::Run(op) = gate.op(op, policy, Duration::from_secs(1)) else {
        panic!("read passes the gate")
    };
    let answer = sidecar.op(&op).unwrap();
    assert!(answer.ok);
    assert_eq!(answer.result["text"], "hi");
    let read = fake.requests("read");
    assert_eq!(read[0]["profile"], COMPANION_PROFILE);
    assert_eq!(read[0]["target"], "T1");
    assert_ne!(read[0]["id"], 77, "the sidecar's id is the helper's own");
}

#[test]
fn two_missed_pings_restart_it_and_it_attaches_again_to_the_same_companion() {
    let fake = Fake::new(|_| None);
    let (sidecar, hooks) = rig(&fake, options());
    sidecar.attach_companion(9333, "nonce-1").unwrap();
    let first = sidecar.pid().unwrap();
    sidecar.tick(Instant::now());
    assert!(hooks.lost.lock().unwrap().is_empty(), "answered pings");
    fake.mute.store(true, Ordering::Relaxed);
    sidecar.tick(Instant::now());
    assert_eq!(sidecar.pid(), Some(first), "one missed ping is no restart");
    sidecar.tick(Instant::now());
    assert_eq!(sidecar.pid(), None);
    assert_eq!(fake.kills.lock().unwrap().clone(), vec![first]);
    assert_eq!(
        hooks.lost.lock().unwrap().clone(),
        vec!["the browser sidecar missed 2 pings; restarted".to_string()]
    );
    fake.mute.store(false, Ordering::Relaxed);
    sidecar.tick(Instant::now());
    assert_eq!(sidecar.pid(), None, "the restart waits its backoff");
    sidecar.tick(Instant::now() + Duration::from_secs(2));
    assert!(sidecar.pid().is_some_and(|pid| pid != first));
    assert!(sidecar.attached());
    let attaches = fake.requests("attach");
    assert_eq!(attaches.len(), 2);
    assert_eq!(attaches[1]["args"], attaches[0]["args"]);
    assert_eq!(hooks.attached.load(Ordering::Relaxed), 2);
}

#[test]
fn an_answer_line_of_45_mb_arrives_and_pings_go_on() {
    const BIG: usize = 45 * 1024 * 1024;
    let fake = Fake::new(|request| {
        (request["op"] == "screenshot").then(|| {
            vec![reply(
                request,
                json!({ "full": "A".repeat(BIG), "bytes": BIG }),
            )]
        })
    });
    let (sidecar, hooks) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    let answer = sidecar
        .request("screenshot", Some("T1"), json!({}), Duration::from_secs(30))
        .unwrap();
    assert!(answer.ok);
    assert_eq!(answer.result["full"].as_str().map(str::len), Some(BIG));
    sidecar.tick(Instant::now());
    sidecar.tick(Instant::now());
    assert!(hooks.lost.lock().unwrap().is_empty());
}

#[test]
fn a_line_over_the_limit_is_dropped_and_the_reader_goes_on() {
    let fake = Fake::new(|request| {
        (request["op"] == "huge").then(|| vec![reply(request, json!({ "x": "B".repeat(4096) }))])
    });
    let (sidecar, hooks) = rig(
        &fake,
        SidecarOptions {
            line_max: 1024,
            ..options()
        },
    );
    sidecar.attach_companion(9333, "n").unwrap();
    let err = sidecar
        .inner
        .call(
            "huge",
            Some(COMPANION_PROFILE),
            None,
            Value::Null,
            Duration::from_millis(100),
            Duration::from_millis(300),
        )
        .unwrap_err();
    assert_eq!(err.code, "browser_timeout");
    sidecar.tick(Instant::now());
    sidecar.tick(Instant::now());
    assert!(
        hooks.lost.lock().unwrap().is_empty(),
        "pings still answered"
    );
    assert!(sidecar.attached());
}

#[test]
fn events_pass_on_as_they_came() {
    let tab = json!({ "event": "tab", "profile": "companion", "target": "T", "kind": "navigated", "url": "https://example.com/", "later_field": [1, 2] });
    let gone =
        json!({ "event": "browser", "profile": "companion", "kind": "disconnected", "detail": "" });
    let lines = vec![tab.to_string(), "not json".to_string(), gone.to_string()];
    let fake = Fake::new(move |request| {
        (request["op"] == "emit").then(|| {
            let mut out = lines.clone();
            out.push(reply(request, json!({})));
            out
        })
    });
    let (sidecar, hooks) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    sidecar
        .request("emit", None, Value::Null, Duration::from_secs(5))
        .unwrap();
    assert_eq!(hooks.events.lock().unwrap().clone(), vec![tab, gone]);
    assert!(!sidecar.attached(), "the companion's CDP connection closed");
}

#[test]
fn a_crash_fails_its_calls_and_says_so_once_and_five_in_five_minutes_give_up() {
    let fake = Fake::new(|request| (request["op"] == "hang").then(Vec::new));
    let (sidecar, hooks) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    let pid = sidecar.pid().unwrap();
    let calling = {
        let inner = Arc::clone(&sidecar.inner);
        std::thread::spawn(move || {
            inner.call(
                "hang",
                None,
                None,
                Value::Null,
                Duration::from_secs(5),
                Duration::from_secs(5),
            )
        })
    };
    wait_for("the hanging call", || !fake.requests("hang").is_empty());
    fake.crash(pid);
    assert_eq!(calling.join().unwrap().unwrap_err().code, "host_failed");
    wait_for("the drop", || sidecar.pid().is_none());
    assert_eq!(
        hooks.lost.lock().unwrap().clone(),
        vec!["the browser sidecar exited".to_string()]
    );
    let mut now = Instant::now();
    for n in 2..=5usize {
        now += Duration::from_secs(20);
        sidecar.tick(now);
        wait_for("the restart", || sidecar.attached());
        fake.crash(sidecar.pid().unwrap());
        wait_for("the drop", || hooks.lost.lock().unwrap().len() == n);
    }
    sidecar.tick(now + Duration::from_secs(3600));
    assert_eq!(
        sidecar.pid(),
        None,
        "five drops: no more restarts by themselves"
    );
    assert_eq!(fake.spawns.load(Ordering::Relaxed), 5);
    // the companion registering again tries once more
    sidecar.attach_companion(9333, "n").unwrap();
    assert_eq!(fake.spawns.load(Ordering::Relaxed), 6);
}

#[test]
fn shutdown_closes_its_stdin() {
    let fake = Fake::new(|_| None);
    let (sidecar, hooks) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    sidecar.shutdown();
    wait_for("the end of its stdin", || {
        fake.eofs.load(Ordering::Relaxed) == 1
    });
    assert!(fake.kills.lock().unwrap().is_empty());
    assert!(
        hooks.lost.lock().unwrap().is_empty(),
        "a shutdown is no loss"
    );
    assert_eq!(
        sidecar
            .request("read", None, Value::Null, Duration::from_secs(1))
            .unwrap_err()
            .code,
        "host_failed"
    );
    sidecar.tick(Instant::now() + Duration::from_secs(3600));
    assert_eq!(fake.spawns.load(Ordering::Relaxed), 1);
}

#[test]
fn the_quit_is_sent_and_not_waited_for() {
    let fake = Fake::new(|_| None);
    let (sidecar, _) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    let started = Instant::now();
    CompanionLink::quit(&sidecar);
    assert!(started.elapsed() < Duration::from_secs(1));
    wait_for("the quit", || !fake.requests("control_quit").is_empty());
    assert_eq!(
        fake.requests("control_quit")[0]["profile"],
        COMPANION_PROFILE
    );
    CompanionLink::gone(&sidecar);
    assert!(!sidecar.attached());
}

#[test]
fn lines_are_read_whole_up_to_the_limit() {
    let mut input: &[u8] = b"abc\n\nxxxxxxxxxx\nlast";
    let mut line = Vec::new();
    assert_eq!(
        read_line_capped(&mut input, 8, &mut line).unwrap(),
        Some(true)
    );
    assert_eq!(line, b"abc\n");
    assert_eq!(
        read_line_capped(&mut input, 8, &mut line).unwrap(),
        Some(true)
    );
    assert_eq!(line, b"\n");
    assert_eq!(
        read_line_capped(&mut input, 8, &mut line).unwrap(),
        Some(false)
    );
    assert_eq!(
        read_line_capped(&mut input, 8, &mut line).unwrap(),
        Some(true)
    );
    assert_eq!(line, b"last");
    assert_eq!(read_line_capped(&mut input, 8, &mut line).unwrap(), None);
}

#[test]
fn a_failed_attach_of_the_new_sidecar_is_tried_again_on_the_backoff() {
    let refuse = Arc::new(AtomicBool::new(false));
    let refusing = Arc::clone(&refuse);
    let fake = Fake::new(move |request| {
        (request["op"] == "attach" && refusing.load(Ordering::Relaxed)).then(|| {
            vec![json!({ "id": request["id"], "ok": false, "error": { "code": "host_verify_failed", "message": "a dialog is open" } }).to_string()]
        })
    });
    let (sidecar, hooks) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    refuse.store(true, Ordering::Relaxed);
    fake.crash(sidecar.pid().unwrap());
    wait_for("the drop", || sidecar.pid().is_none());
    let now = Instant::now();
    sidecar.tick(now + Duration::from_secs(2));
    assert!(sidecar.pid().is_some(), "a new sidecar runs");
    assert!(!sidecar.attached(), "its attach was refused");
    refuse.store(false, Ordering::Relaxed);
    sidecar.tick(now + Duration::from_secs(2));
    assert!(!sidecar.attached(), "the next try waits its backoff");
    sidecar.tick(now + Duration::from_secs(10));
    assert!(sidecar.attached());
    assert_eq!(
        fake.spawns.load(Ordering::Relaxed),
        2,
        "the same new sidecar"
    );
    assert_eq!(fake.requests("attach").len(), 3);
    assert_eq!(hooks.attached.load(Ordering::Relaxed), 2);
}

#[test]
fn a_closed_cdp_connection_is_attached_again() {
    let fake = Fake::new(move |request| {
        (request["op"] == "emit").then(|| {
            vec![
                json!({ "event": "browser", "profile": "companion", "kind": "disconnected", "detail": "" }).to_string(),
                reply(request, json!({})),
            ]
        })
    });
    let (sidecar, _) = rig(&fake, options());
    sidecar.attach_companion(9333, "n").unwrap();
    sidecar
        .request("emit", None, Value::Null, Duration::from_secs(5))
        .unwrap();
    assert!(!sidecar.attached());
    sidecar.tick(Instant::now());
    assert!(sidecar.attached());
    assert_eq!(fake.requests("attach").len(), 2);
    assert_eq!(fake.spawns.load(Ordering::Relaxed), 1);
}
