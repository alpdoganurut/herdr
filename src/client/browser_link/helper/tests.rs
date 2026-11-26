//! The helper against a fake server end (frames over a socketpair), a fake
//! sidecar and a fake companion; the companion's API through the real socket.

use std::io::{BufRead as _, BufReader};
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

use super::*;
use crate::api::schema::BrowserPlacementSpec;
use crate::browser::link_proto::{BlobAck, BlobName, Ensure, LinkOp};

const BOOT: &str = "boot-a";

fn wait_for(what: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !what() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Fakes

/// Ops wait here until the test answers them (or five seconds pass).
#[derive(Default)]
struct FakeSidecar {
    sent: Mutex<Vec<Value>>,
    waiting: Mutex<HashMap<u64, mpsc::Sender<HostReply>>>,
}

impl FakeSidecar {
    fn sent(&self) -> Vec<Value> {
        lock(&self.sent).clone()
    }

    /// The `nth` request of op `name` (its `id` is the fake's number).
    fn wait_op(&self, name: &str, nth: usize) -> Value {
        wait_for(|| self.sent().iter().filter(|r| r["op"] == name).count() > nth);
        self.sent()
            .into_iter()
            .filter(|r| r["op"] == name)
            .nth(nth)
            .unwrap_or_default()
    }

    fn reply(&self, id: u64, result: Value) {
        let waiting = lock(&self.waiting).remove(&id);
        if let Some(waiting) = waiting {
            let _ = waiting.send(HostReply {
                id,
                ok: true,
                result,
                error: None,
                page: None,
            });
        }
    }
}

impl SidecarOps for FakeSidecar {
    fn op(&self, op: &GatedOp) -> Result<HostReply, BrowserError> {
        let (tx, rx) = mpsc::channel();
        {
            let mut sent = lock(&self.sent);
            let id = sent.len() as u64 + 1;
            sent.push(serde_json::to_value(op.request(id)).unwrap_or_default());
            lock(&self.waiting).insert(id, tx);
        }
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| BrowserError::new("browser_timeout", "no answer in this test"))
    }
    fn shutdown(&self) {}
}

type OnEnsure = Box<dyn Fn() + Send>;

#[derive(Default)]
struct FakeCompanion {
    calls: Mutex<Vec<String>>,
    on_ensure: Mutex<Option<OnEnsure>>,
}

impl FakeCompanion {
    fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }
}

impl CompanionControl for FakeCompanion {
    fn recover(&self) {
        lock(&self.calls).push("recover".into());
    }
    fn instance(&self) -> String {
        "inst-1".into()
    }
    fn readiness(&self) -> (BrowserLinkReady, Option<String>) {
        (BrowserLinkReady::Ready, None)
    }
    fn ensure(&self, _wait: Duration) -> Result<(), LinkError> {
        lock(&self.calls).push("ensure".into());
        if let Some(start) = lock(&self.on_ensure).as_ref() {
            start();
        }
        Ok(())
    }
    fn register(&self, params: &BrowserHostRegisterParams) -> Result<Value, BrowserError> {
        assert_eq!((params.cdp.port, params.nonce.as_str()), (9222, "n1"));
        let token = params.launch_token.clone().unwrap_or_default();
        lock(&self.calls).push(format!("register {token}"));
        Ok(json!({"tabs": [{"target": "T1", "url": "https://example.com/"}]}))
    }
    fn heartbeat(&self, _launch_token: &str) -> bool {
        true
    }
    fn unregistered(&self, launch_token: &str) {
        lock(&self.calls).push(format!("unregistered {launch_token}"));
    }
    fn start(&self) -> Result<(), String> {
        lock(&self.calls).push("start".into());
        Ok(())
    }
    fn stop(&self) {
        lock(&self.calls).push("stop".into());
    }
    fn restart(&self) -> Result<(), String> {
        lock(&self.calls).push("restart".into());
        Ok(())
    }
    fn focus(&self) -> Result<(), String> {
        lock(&self.calls).push("focus".into());
        Ok(())
    }
    fn forget(&self) -> io::Result<()> {
        lock(&self.calls).push("forget".into());
        Ok(())
    }
    fn shutdown(&self) {
        lock(&self.calls).push("shutdown".into());
    }
    fn info(&self) -> BrowserCompanionAppInfo {
        BrowserCompanionAppInfo {
            state: "running".into(),
            electron: Some("39.0.0".into()),
            update_ready: true,
            update: Some("0.2.14".into()),
            ..Default::default()
        }
    }
}

/// Every open hands the test the server's end.
struct Pairs(Mutex<mpsc::Sender<UnixStream>>);

impl LinkTransport for Pairs {
    fn open(&self) -> io::Result<LinkConnection> {
        let (ours, theirs) = UnixStream::pair()?;
        lock(&self.0)
            .send(theirs)
            .map_err(|_| io::Error::other("the test is over"))?;
        let ender = ours.try_clone()?;
        Ok(LinkConnection {
            reader: Box::new(ours.try_clone()?),
            writer: Box::new(ours),
            end: Box::new(move || {
                let _ = ender.shutdown(std::net::Shutdown::Both);
            }),
        })
    }
}

/// The server's end of one link.
struct B {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    /// A line a read timeout cut short.
    partial: Vec<u8>,
    last_ping: u64,
}

impl B {
    fn new(stream: UnixStream) -> Self {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
            partial: Vec::new(),
            last_ping: 0,
        }
    }

    /// The next whole line; `None` at the end or a read timeout (what came
    /// of a line so far is kept for the next call).
    fn line(&mut self) -> Option<Vec<u8>> {
        match self.reader.read_until(b'\n', &mut self.partial) {
            Ok(_) if self.partial.ends_with(b"\n") => Some(std::mem::take(&mut self.partial)),
            Ok(_) | Err(_) => None,
        }
    }

    fn write(&mut self, line: &str) {
        let _ = self.stream.write_all(line.as_bytes());
    }

    /// Reads the open request and answers it with an epoch.
    fn open(&mut self, epoch: u64) -> Value {
        let request: Value = serde_json::from_slice(&self.line().unwrap()).unwrap();
        assert_eq!(request["method"], "browser.link_open");
        let answer = crate::api::schema::SuccessResponse {
            id: request["id"].as_str().unwrap().to_owned(),
            result: crate::api::schema::ResponseResult::BrowserLinkOpened {
                link: BrowserLinkInfo {
                    link_id: "K1".into(),
                    epoch,
                    boot_id: BOOT.into(),
                    link_protocol: 1,
                    features: Vec::new(),
                    ping_ms: 3000,
                    ttl_ms: 15000,
                },
            },
        };
        self.write(&format!("{}\n", serde_json::to_string(&answer).unwrap()));
        request["params"].clone()
    }

    fn refuse(&mut self, code: &str, message: &str) {
        let request: Value = serde_json::from_slice(&self.line().unwrap()).unwrap();
        let answer = crate::api::schema::ErrorResponse {
            id: request["id"].as_str().unwrap().to_owned(),
            error: crate::api::schema::ErrorBody {
                code: code.into(),
                message: message.into(),
            },
        };
        let answer = serde_json::to_string(&answer).unwrap();
        self.write(&format!("{answer}\n"));
    }

    fn send(&mut self, frame: &ServerFrame) {
        let line = frame.encode().unwrap();
        self.write(&line);
    }

    /// The next frame that is no ping (pings are answered).
    fn frame(&mut self) -> Option<HelperFrame> {
        loop {
            let frame = HelperFrame::decode(&self.line()?).unwrap();
            match frame {
                HelperFrame::Ping { n } => {
                    self.last_ping = n;
                    self.send(&ServerFrame::Pong { n });
                }
                frame => return Some(frame),
            }
        }
    }

    /// A frame that is no ping within `wait`, else `None`.
    fn frame_within(&mut self, wait: Duration) -> Option<HelperFrame> {
        let deadline = Instant::now() + wait;
        self.stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            found = self.frame();
        }
        self.stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        found
    }

    /// Waits for a ping newer than the last one, so an op can echo it.
    fn ping(&mut self) -> u64 {
        let seen = self.last_ping;
        loop {
            let frame = HelperFrame::decode(&self.line().unwrap()).unwrap();
            if let HelperFrame::Ping { n } = frame {
                self.last_ping = n;
                self.send(&ServerFrame::Pong { n });
                if n > seen {
                    return n;
                }
            } else {
                panic!("a frame while waiting for a ping: {frame:?}");
            }
        }
    }

    fn op(&mut self, id: u64, op: &str, args: Value) {
        let echo = self.last_ping;
        self.send(&ServerFrame::Op(LinkOp {
            id,
            echo,
            op: op.into(),
            target: Some("T1".into()),
            args,
            deadline_ms: 5_000,
            activity: None,
        }));
    }
}

struct Rig {
    helper: Arc<Helper>,
    hooks: Arc<Hooks>,
    ends: mpsc::Receiver<UnixStream>,
    sidecar: Arc<FakeSidecar>,
    companion: Arc<FakeCompanion>,
    links: Arc<Mutex<Vec<bool>>>,
    thread: Option<JoinHandle<()>>,
}

fn timing() -> Timing {
    Timing {
        ping: Duration::from_millis(100),
        silence: Duration::from_millis(1_500),
        first_retry: Duration::from_millis(50),
        max_retry: Duration::from_millis(200),
        slow_retry: Duration::from_secs(2),
        steady_retry: Duration::from_secs(4),
        answer: Duration::from_secs(2),
        tick: Duration::from_millis(20),
        idle_exit: Duration::from_millis(400),
        lease_ttl: Duration::from_secs(15),
        host_ok_wait: Duration::from_secs(2),
        ensure: Duration::from_secs(3),
    }
}

impl Rig {
    fn new() -> Self {
        Self::build(true)
    }

    fn build(link: bool) -> Self {
        let (ends_tx, ends) = mpsc::channel();
        let sidecar = Arc::new(FakeSidecar::default());
        let companion = Arc::new(FakeCompanion::default());
        let links: Arc<Mutex<Vec<bool>>> = Arc::default();
        let start = Instant::now();
        let helper = Helper::new(Deps {
            label: "mbp".into(),
            machine: "buildbox".into(),
            host_id: "h".repeat(32),
            herdr: "test".into(),
            companion: Arc::clone(&companion) as Arc<dyn CompanionControl>,
            sidecar: Arc::clone(&sidecar) as Arc<dyn SidecarOps>,
            transport: Arc::new(Pairs(Mutex::new(ends_tx))),
            policy: Arc::new(|| Policy {
                allow_eval: true,
                allow_password: false,
            }),
            clock: Arc::new(move || start.elapsed()),
            on_link: {
                let links = Arc::clone(&links);
                Arc::new(move |up| lock(&links).push(up))
            },
            timing: timing(),
        });
        let hooks = Arc::new(Hooks::default());
        hooks.bind(&helper);
        let thread = link.then(|| {
            let helper = Arc::clone(&helper);
            std::thread::spawn(move || helper.link_loop())
        });
        Self {
            helper,
            hooks,
            ends,
            sidecar,
            companion,
            links,
            thread,
        }
    }

    fn b(&self) -> B {
        B::new(self.ends.recv_timeout(Duration::from_secs(5)).unwrap())
    }

    fn no_open_within(&self, wait: Duration) -> bool {
        self.ends.recv_timeout(wait).is_err()
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, (String, String)> {
        call(&self.helper, method, params)
    }

    /// Registers on a thread; `b` answers its host_up.
    fn register(&self, b: &mut B) -> Value {
        let helper = Arc::clone(&self.helper);
        let registering =
            std::thread::spawn(move || call(&helper, "browser.host_register", register_params()));
        let Some(HelperFrame::HostUp(up)) = b.frame() else {
            panic!("no host_up");
        };
        assert_eq!(up.instance, "app-1");
        assert_eq!(up.electron, "39.0.0");
        assert_eq!(up.tabs.len(), 1);
        b.send(&ServerFrame::HostOk {
            lease_id: "LB".into(),
        });
        let Some(HelperFrame::Api(subscribe)) = b.frame() else {
            panic!("no upstream subscribe");
        };
        assert_eq!(subscribe.method, "events.subscribe");
        registering.join().unwrap().unwrap()["lease"].clone()
    }

    /// A link with a registered companion: the server's end and the lease.
    fn linked() -> (Self, B, String) {
        let rig = Self::new();
        let mut b = rig.b();
        b.open(1);
        // the reader opens the session once it read the answer
        wait_for(|| rig.helper.state().session.is_some());
        let lease = rig.register(&mut b);
        assert_eq!(lease["link"], json!({"state": "linked"}));
        let lease_id = lease["lease_id"].as_str().unwrap().to_owned();
        b.ping();
        (rig, b, lease_id)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.helper.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn call(helper: &Helper, method: &str, params: Value) -> Result<Value, (String, String)> {
    match helper.call(method, params) {
        Answer::Result(result) | Answer::Stream(result, _) => Ok(result),
        Answer::Error { code, message } => Err((code, message)),
    }
}

fn register_params() -> Value {
    json!({"kind": "companion", "instance": "app-1", "cdp": {"host": "127.0.0.1", "port": 9222},
           "nonce": "n1", "version": "0.2.13", "protocol": 1, "launch_token": "tok"})
}

fn stream(helper: &Helper) -> mpsc::Receiver<String> {
    match helper.call("events.subscribe", json!({})) {
        Answer::Stream(first, lines) => {
            assert_eq!(first["type"], "subscription_started");
            lines
        }
        _ => panic!("no stream"),
    }
}

fn ack(b: &mut B, chunk: &crate::browser::link_proto::BlobChunk) {
    b.send(&ServerFrame::BlobAck(BlobAck {
        op: chunk.op,
        name: chunk.name,
        seq: chunk.seq,
    }));
}

// ---------------------------------------------------------------------------
// The link

#[test]
fn a_link_runs_gated_ops_with_its_ping_numbers() {
    let (rig, mut b, _) = Rig::linked();
    assert_eq!(lock(&rig.links).as_slice(), &[true]);
    assert_eq!(rig.companion.calls(), vec!["register tok"]);

    b.op(7, "read", json!({"max_chars": 10}));
    let request = rig.sidecar.wait_op("read", 0);
    assert_eq!(request["profile"], "companion");
    assert_eq!(request["target"], "T1");
    rig.sidecar
        .reply(request["id"].as_u64().unwrap(), json!({"text": "hi"}));
    let Some(HelperFrame::Re(re)) = b.frame() else {
        panic!("no re");
    };
    assert_eq!((re.id, re.ok, re.result), (7, true, json!({"text": "hi"})));

    // an op that echoes a ping never sent is stale: nothing runs
    b.send(&ServerFrame::Op(LinkOp {
        id: 8,
        echo: 9_999,
        op: "act".into(),
        target: Some("T1".into()),
        args: json!({"kind": "click", "ref": "e1"}),
        deadline_ms: 5_000,
        activity: None,
    }));
    let Some(HelperFrame::Re(re)) = b.frame() else {
        panic!("no re");
    };
    assert_eq!(re.error.unwrap().code, code::HOST_LINK_LOST);
    // refused by the gate, or answered by the helper
    b.op(9, "attach", json!({}));
    let Some(HelperFrame::Re(re)) = b.frame() else {
        panic!("no re");
    };
    assert_eq!(re.error.unwrap().code, code::HOST_REFUSED);
    b.op(10, "hello", Value::Null);
    let Some(HelperFrame::Re(re)) = b.frame() else {
        panic!("no re");
    };
    assert_eq!(re.result["host_protocol"], 1);
    assert_eq!(
        rig.sidecar.sent().len(),
        1,
        "only the read reached the sidecar"
    );
}

#[test]
fn the_open_says_who_and_what() {
    let rig = Rig::new();
    let params = rig.b().open(1);
    assert_eq!(params["link_protocol"], 1);
    assert_eq!(params["host_id"], "h".repeat(32));
    assert_eq!(params["instance"], "inst-1");
    assert_eq!(params["label"], "mbp");
    assert_eq!(params["ready"], "ready");
}

#[test]
fn a_drop_mid_op_answers_every_waiter_and_an_old_epoch_stays_out() {
    let (rig, mut b, lease_id) = Rig::linked();
    let events = stream(&rig.helper);
    b.op(3, "read", json!({}));
    let old = rig.sidecar.wait_op("read", 0);

    let helper = Arc::clone(&rig.helper);
    let lease = lease_id.clone();
    let relayed =
        std::thread::spawn(move || call(&helper, "browser.host_state", json!({"lease_id": lease})));
    let Some(HelperFrame::Api(api)) = b.frame() else {
        panic!("not relayed");
    };
    assert_eq!(api.method, "browser.host_state");
    assert!(
        api.params.get("lease_id").is_none(),
        "B puts its own lease in"
    );
    let helper = Arc::clone(&rig.helper);
    let pulled = std::thread::spawn(move || {
        call(
            &helper,
            "link.file",
            json!({"token": "a".repeat(24), "path": "index.html", "offset": 0}),
        )
    });
    let Some(HelperFrame::File(_)) = b.frame() else {
        panic!("no pull");
    };

    drop(b);
    let at = Instant::now();
    assert_eq!(relayed.join().unwrap().unwrap_err().0, code::LINK_DOWN);
    assert_eq!(pulled.join().unwrap().unwrap_err().0, code::LINK_DOWN);
    assert!(at.elapsed() < Duration::from_secs(1));
    // while down: at once, and the lease says so
    let at = Instant::now();
    let down = rig
        .call("browser.host_sync", json!({"lease_id": lease_id}))
        .unwrap_err();
    assert_eq!(down.0, code::LINK_DOWN);
    assert!(at.elapsed() < Duration::from_millis(200));
    let lease = rig
        .call("browser.host_heartbeat", json!({"lease_id": lease_id}))
        .unwrap();
    assert_eq!(lease["lease"]["link"]["state"], "reconnecting");
    assert_eq!(lock(&rig.links).as_slice(), &[true, false]);

    // the next epoch: host_up again first, link_up after host_ok
    let mut b = rig.b();
    b.open(2);
    let Some(HelperFrame::HostUp(up)) = b.frame() else {
        panic!("no host_up");
    };
    assert_eq!(up.instance, "app-1");
    b.send(&ServerFrame::HostOk {
        lease_id: "LB2".into(),
    });
    let Some(HelperFrame::Api(subscribe)) = b.frame() else {
        panic!("no subscribe");
    };
    assert_eq!(subscribe.method, "events.subscribe");
    let up: Value =
        serde_json::from_str(&events.recv_timeout(Duration::from_secs(2)).unwrap()).unwrap();
    assert_eq!(up, json!({"event": "herdr.link_up", "data": {}}));
    // the old epoch's answer goes nowhere
    rig.sidecar
        .reply(old["id"].as_u64().unwrap(), json!({"text": "old"}));
    b.ping();
    b.op(4, "read", json!({}));
    let new = rig.sidecar.wait_op("read", 1);
    rig.sidecar
        .reply(new["id"].as_u64().unwrap(), json!({"text": "new"}));
    let Some(HelperFrame::Re(re)) = b.frame() else {
        panic!("no re");
    };
    assert_eq!(re.id, 4);
    // B's events reach the stream; B's lease_unknown never reaches the companion
    b.send(&ServerFrame::Event {
        event: json!({"event": "tab.focused", "data": {"tab_id": "w1:t1"}}),
    });
    let line: Value =
        serde_json::from_str(&events.recv_timeout(Duration::from_secs(2)).unwrap()).unwrap();
    assert_eq!(line["event"], "tab.focused");
    let helper = Arc::clone(&rig.helper);
    let relayed =
        std::thread::spawn(move || call(&helper, "tab.focus", json!({"tab_id": "w1:t1"})));
    let Some(HelperFrame::Api(api)) = b.frame() else {
        panic!("not relayed");
    };
    b.send(&ServerFrame::ApiRe(ApiReply {
        id: api.id,
        ok: false,
        result: Value::Null,
        error: Some(LinkError::new("lease_unknown", "no such lease")),
    }));
    assert_eq!(relayed.join().unwrap().unwrap_err().0, code::LINK_DOWN);
}

#[test]
fn replaced_and_refusals_are_not_reopened_fast() {
    let rig = Rig::new();
    let mut b = rig.b();
    b.open(1);
    b.send(&ServerFrame::Bye(Bye {
        code: bye::REPLACED.into(),
        message: "this Mac opened a newer link".into(),
    }));
    assert!(rig.no_open_within(Duration::from_millis(800)));

    let rig = Rig::new();
    rig.b()
        .refuse(code::LINK_DISABLED, "this herdr takes no browser link");
    assert!(rig.no_open_within(Duration::from_millis(800)));
    let view = rig.helper.view();
    assert_eq!(view.link, "off");
    assert_eq!(
        view.text.as_deref(),
        Some("this herdr takes no browser link")
    );

    let rig = Rig::new();
    let text = "the browser of buildbox is on workstation (since 14:02); quit herdr+ there";
    rig.b().refuse(code::LINK_IN_USE, text);
    assert!(rig.no_open_within(Duration::from_millis(800)));
    assert_eq!(rig.helper.view().owner.as_deref(), Some(text));

    // a link that served is opened again at once after it dropped
    let (rig, b, _) = Rig::linked();
    drop(b);
    assert!(!rig.no_open_within(Duration::from_secs(1)));
}

#[test]
fn a_server_that_stops_reading_is_dropped_and_the_blocked_writer_freed() {
    let (rig, mut b, _) = Rig::linked();
    b.op(5, "screenshot", json!({}));
    let request = rig.sidecar.wait_op("screenshot", 0);
    // four chunks of a large picture fill the socket while B reads nothing
    let full = vec![7u8; 2 * 1024 * 1024];
    rig.sidecar.reply(
        request["id"].as_u64().unwrap(),
        json!({"full": STANDARD.encode(&full)}),
    );
    let at = Instant::now();
    let next = rig.ends.recv_timeout(Duration::from_secs(6));
    assert!(next.is_ok(), "opened again after the silence");
    let waited = at.elapsed();
    assert!(waited >= Duration::from_millis(1_000), "{waited:?}");
    assert!(waited < Duration::from_millis(4_000), "{waited:?}");
    // the old link's end: what was written, then the end
    let mut rest = Vec::new();
    std::io::Read::read_to_end(&mut b.reader, &mut rest).unwrap();
}

#[test]
fn ensure_answers_after_host_up_and_a_stale_one_is_refused() {
    let rig = Rig::new();
    let mut b = rig.b();
    b.open(1);
    let echo = b.ping();
    {
        let helper = Arc::clone(&rig.helper);
        *lock(&rig.companion.on_ensure) = Some(Box::new(move || {
            let helper = Arc::clone(&helper);
            std::thread::spawn(move || call(&helper, "browser.host_register", register_params()));
        }));
    }
    b.send(&ServerFrame::Ensure(Ensure {
        id: 1,
        echo,
        reason: "open".into(),
    }));
    let Some(HelperFrame::HostUp(_)) = b.frame() else {
        panic!("host_up first");
    };
    let Some(HelperFrame::EnsureRe(re)) = b.frame() else {
        panic!("then ensure_re");
    };
    assert!(re.ok && re.id == 1);
    b.send(&ServerFrame::Ensure(Ensure {
        id: 2,
        echo: 777,
        reason: "open".into(),
    }));
    let Some(HelperFrame::EnsureRe(re)) = b.frame() else {
        panic!("no ensure_re");
    };
    assert!(!re.ok);
    assert_eq!(re.error.unwrap().code, code::HOST_LINK_LOST);
}

#[test]
fn the_sidecar_going_answers_its_ops_before_host_down() {
    let (rig, mut b, lease_id) = Rig::linked();
    let events = stream(&rig.helper);
    b.op(1, "act", json!({"kind": "click", "ref": "e1"}));
    b.op(2, "read", json!({}));
    rig.sidecar.wait_op("read", 0);
    rig.hooks.lost("the sidecar exited");
    let seen: Vec<HelperFrame> = (0..3).map(|_| b.frame().unwrap()).collect();
    let HelperFrame::Re(first) = &seen[0] else {
        panic!("{seen:?}");
    };
    assert_eq!(first.error.as_ref().unwrap().code, code::HOST_LINK_LOST);
    assert!(first
        .error
        .as_ref()
        .unwrap()
        .message
        .contains("may or may not"));
    assert!(matches!(&seen[1], HelperFrame::Re(re) if !re.ok));
    assert!(matches!(&seen[2], HelperFrame::HostDown { reason } if reason == "the sidecar exited"));
    // its answers after that go nowhere
    let act = rig.sidecar.wait_op("act", 0);
    rig.sidecar.reply(act["id"].as_u64().unwrap(), json!({}));
    // B's lease went with host_down: the companion hears so, nothing is relayed
    let lease = rig
        .call("browser.host_heartbeat", json!({"lease_id": lease_id}))
        .unwrap();
    assert_eq!(lease["lease"]["link"]["state"], "reconnecting");
    let down = rig
        .call("browser.host_state", json!({"lease_id": lease_id}))
        .unwrap_err();
    assert_eq!(down.0, code::LINK_DOWN);
    // it comes back by itself while the lease lives
    rig.hooks.attached(&json!({"tabs": []}));
    let Some(HelperFrame::HostUp(up)) = b.frame() else {
        panic!("no host_up");
    };
    assert_eq!(
        (up.instance.as_str(), up.electron.as_str()),
        ("app-1", "39.0.0")
    );
    b.send(&ServerFrame::HostOk {
        lease_id: "LB2".into(),
    });
    let up: Value =
        serde_json::from_str(&events.recv_timeout(Duration::from_secs(2)).unwrap()).unwrap();
    assert_eq!(up["event"], "herdr.link_up");
    assert!(
        b.frame_within(Duration::from_millis(300)).is_none(),
        "no second subscribe on the same link"
    );
    // the companion's exit ends its lease
    rig.hooks.exited("the companion exited");
    assert!(matches!(b.frame(), Some(HelperFrame::HostDown { .. })));
    assert!(rig.helper.state().lease.is_none());
}

#[test]
fn blobs_go_in_a_window_and_the_answer_after_them() {
    use crate::browser::link_proto::{BLOB_WINDOW, CHUNK_BYTES};

    let (rig, mut b, _) = Rig::linked();
    b.op(5, "screenshot", json!({}));
    let request = rig.sidecar.wait_op("screenshot", 0);
    let full: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    rig.sidecar.reply(
        request["id"].as_u64().unwrap(),
        json!({"full": STANDARD.encode(&full), "inline": STANDARD.encode(b"tiny"), "width": 4}),
    );
    let mut open = Vec::new();
    let mut got = Vec::new();
    loop {
        match b.frame_within(Duration::from_millis(300)) {
            Some(HelperFrame::Blob(chunk)) => {
                assert!(chunk.data.len() <= CHUNK_BYTES);
                open.push(chunk);
                assert!(open.len() as u64 <= BLOB_WINDOW);
            }
            Some(HelperFrame::Re(re)) => {
                assert!(open.is_empty(), "the answer came before its last ack");
                assert_eq!(re.id, 5);
                assert_eq!(re.result, json!({"width": 4}));
                break;
            }
            Some(other) => panic!("{other:?}"),
            None => {
                // nothing more until an ack: the window is full or all is out
                let full_window = open.len() as u64 == BLOB_WINDOW;
                let all_out = open.iter().any(|c| c.name == BlobName::Inline && c.last);
                assert!(full_window || all_out, "stalled with {} open", open.len());
                let chunk = open.remove(0);
                ack(&mut b, &chunk);
                got.push(chunk);
            }
        }
    }
    let full_back: Vec<u8> = got
        .iter()
        .filter(|c| c.name == BlobName::Full)
        .flat_map(|c| c.data.clone())
        .collect();
    assert_eq!(full_back, full);
    assert!(got
        .iter()
        .any(|c| c.name == BlobName::Inline && c.data == b"tiny"));
}

#[test]
fn file_pulls_go_four_at_a_time() {
    use crate::browser::link_proto::FILE_WINDOW;

    let (rig, mut b, _) = Rig::linked();
    let pulls: Vec<_> = (0..6u64)
        .map(|offset| {
            let helper = Arc::clone(&rig.helper);
            std::thread::spawn(move || {
                call(
                    &helper,
                    "link.file",
                    json!({"token": "a".repeat(24), "path": "a%20b.html", "offset": offset}),
                )
            })
        })
        .collect();
    let mut asked = Vec::new();
    while let Some(HelperFrame::File(request)) = b.frame_within(Duration::from_millis(400)) {
        asked.push(request);
    }
    assert_eq!(asked.len(), FILE_WINDOW);
    let mut answered = 0;
    while let Some(request) = asked.pop() {
        assert_eq!(request.path, "a%20b.html");
        b.send(&ServerFrame::FileRe(FileReply {
            id: request.id,
            status: 200,
            mime: "text/html".into(),
            size: 6,
            data: vec![b'x'; 1],
            eof: false,
        }));
        answered += 1;
        if let Some(HelperFrame::File(more)) = b.frame_within(Duration::from_millis(200)) {
            asked.push(more);
        }
    }
    assert_eq!(answered, 6);
    for pull in pulls {
        let answer = pull.join().unwrap().unwrap();
        assert_eq!(answer["status"], 200);
        assert_eq!(answer["data"], STANDARD.encode(b"x"));
        assert_eq!(answer["size"], 6);
    }
}

#[test]
fn the_lease_goes_when_it_is_not_renewed() {
    let (rig, mut b, lease_id) = Rig::linked();
    rig.helper.state().lease.as_mut().unwrap().expires = Duration::ZERO;
    rig.helper.expire_lease();
    assert!(matches!(b.frame(), Some(HelperFrame::HostDown { .. })));
    let gone = rig
        .call("browser.host_heartbeat", json!({"lease_id": lease_id}))
        .unwrap_err();
    assert_eq!(gone.0, "lease_unknown");
}

// ---------------------------------------------------------------------------
// The process: the companion on the API socket, a client on the link socket

#[test]
fn a_companion_and_a_client_on_the_real_sockets() {
    use super::super::clients::{encode, ClientMessage, CLIENT_PROTO};

    let dir = super::super::api_socket::tests::short_dir();
    let (api, link) = (dir.path().join("k.api"), dir.path().join("k.link"));
    let rig = Rig::build(false);
    let serving = {
        let helper = Arc::clone(&rig.helper);
        let (api, link) = (api.clone(), link.clone());
        std::thread::spawn(move || helper.serve(&api, &link))
    };
    wait_for(|| link.exists() && api.exists());
    let ask = |line: Value| -> Value {
        let mut stream = UnixStream::connect(&api).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).unwrap()
    };

    // a client says hello
    let mut client = UnixStream::connect(&link).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let hello = ClientMessage::Hello {
        proto: CLIENT_PROTO,
        herdr: "test".into(),
        machine: super::super::clients::MachineFacts {
            id: "0".repeat(32),
            label: "buildbox".into(),
            target: "buildbox".into(),
            session: "default".into(),
        },
        cycle_keys: vec!["ctrl+shift+b".into()],
    };
    client
        .write_all(encode(&hello).unwrap().as_bytes())
        .unwrap();
    let mut from_helper = BufReader::new(client.try_clone().unwrap());
    let mut next = |what: &dyn Fn(&Value) -> bool| -> Value {
        loop {
            let mut line = String::new();
            from_helper.read_line(&mut line).unwrap();
            let message: Value = serde_json::from_str(&line).unwrap();
            if what(&message) {
                return message;
            }
        }
    };
    let state = next(&|m| m["t"] == "state");
    assert_eq!(state["companion"]["state"], "running");
    assert!(matches!(
        state["link"].as_str(),
        Some("connecting" | "reconnecting")
    ));

    let registered =
        ask(json!({"id": "c1", "method": "browser.host_register", "params": register_params()}));
    assert_eq!(registered["id"], "c1");
    assert_eq!(registered["result"]["type"], "browser_host_lease");
    let lease = &registered["result"]["lease"];
    assert_eq!(
        lease["link"]["state"], "reconnecting",
        "no link in this test"
    );
    assert_eq!(lease["view_cycle_keys"], json!(["ctrl+shift+b"]));
    assert_eq!(lease["profile"], "companion");
    assert_eq!(lease["update"], "0.2.14");
    assert!(lease["server_features"]
        .as_array()
        .unwrap()
        .contains(&json!("placements")));
    let lease_id = lease["lease_id"].as_str().unwrap().to_owned();

    // pids that are not its own are not taken
    let spec = BrowserPlacementSpec {
        id: "p1".into(),
        tab_id: "w1:t1".into(),
        pids: vec![1, 2, 3],
        shown: true,
        ..Default::default()
    };
    let placement = ClientMessage::Placement {
        spec: Some(spec),
        active: true,
        focused: true,
    };
    client
        .write_all(encode(&placement).unwrap().as_bytes())
        .unwrap();
    let at = Instant::now();
    let placements = ask(
        json!({"id": "c2", "method": "browser.placements", "params": {"lease_id": lease_id, "since_rev": 0, "wait_ms": 3000}}),
    );
    assert!(at.elapsed() < Duration::from_secs(3), "the long poll woke");
    let one = &placements["result"]["placements"][0];
    assert_eq!(
        one["placement"]["pids"][0],
        std::process::id(),
        "pids from the socket peer"
    );
    assert_ne!(one["placement"]["pids"], json!([1, 2, 3]));
    assert_eq!(one["placement_actions"], true);
    assert_eq!(one["focused"], true);
    assert_eq!(one["placement"]["shown"], true);

    let report = ask(
        json!({"id": "c3", "method": "browser.placement_report", "params": {"lease_id": lease_id, "id": "p1", "state": "covering"}}),
    );
    assert_eq!(report["result"]["type"], "ok");
    let action = ask(
        json!({"id": "c4", "method": "browser.placement_action", "params": {"lease_id": lease_id, "id": "p1", "action": "promote"}}),
    );
    assert_eq!(action["result"]["type"], "ok");
    let relayed = next(&|m| m["t"] == "placement_action");
    assert_eq!(
        (relayed["id"].clone(), relayed["action"].clone()),
        (json!("p1"), json!("promote"))
    );
    let cycle = ask(
        json!({"id": "c5", "method": "browser.view_cycle", "params": {"lease_id": lease_id, "id": "p1", "tab_id": "w1:t1"}}),
    );
    assert_eq!(cycle["result"]["type"], "ok");
    assert_eq!(next(&|m| m["t"] == "view_cycle")["tab_id"], "w1:t1");
    next(&|m| m["t"] == "state" && m["reports"][0]["state"] == "covering");

    let down =
        ask(json!({"id": "c6", "method": "browser.host_state", "params": {"lease_id": lease_id}}));
    assert_eq!(down["error"]["code"], code::LINK_DOWN);
    let other = ask(json!({"id": "c7", "method": "pane.read", "params": {}}));
    assert_eq!(other["error"]["code"], "invalid_request");
    let wrong =
        ask(json!({"id": "c8", "method": "browser.placements", "params": {"lease_id": "Lnope"}}));
    assert_eq!(wrong["error"]["code"], "lease_unknown");
    let companion =
        ask(json!({"id": "c9", "method": "browser.companion", "params": {"action": "focus"}}));
    assert_eq!(companion["result"]["companion"]["state"], "running");
    assert!(rig.companion.calls().contains(&"focus".to_string()));
    let unregistered = ask(
        json!({"id": "c10", "method": "browser.host_unregister", "params": {"lease_id": lease_id}}),
    );
    assert_eq!(unregistered["result"]["type"], "ok");
    assert!(rig
        .companion
        .calls()
        .contains(&"unregistered tok".to_string()));

    // the last client leaves: the helper ends by itself
    drop(from_helper);
    drop(client);
    serving.join().unwrap().unwrap();
    assert!(!api.exists() && !link.exists());
    assert!(rig.companion.calls().contains(&"shutdown".to_string()));
}

// ---------------------------------------------------------------------------
// The machine's home

#[test]
fn the_users_actions_reach_the_companion() {
    let rig = Rig::build(false);
    let weakly = rig.helper.weakly();
    for action in [
        CompanionAction::Forget,
        CompanionAction::Restart,
        CompanionAction::Unknown,
    ] {
        ClientHandler::companion(&weakly, 1, action);
    }
    wait_for(|| {
        let calls = rig.companion.calls();
        calls.contains(&"forget".to_string()) && calls.contains(&"restart".to_string())
    });
}

#[test]
fn one_helper_per_machine_home() {
    let dir = super::super::api_socket::tests::short_dir();
    let first = lock_home(dir.path()).unwrap();
    assert!(first.is_some());
    assert!(lock_home(dir.path()).unwrap().is_none());
    drop(first);
    assert!(lock_home(dir.path()).unwrap().is_some());

    let id = host_id(dir.path()).unwrap();
    assert_eq!(id.len(), 32);
    assert_eq!(host_id(dir.path()).unwrap(), id);
    let mode = std::fs::metadata(dir.path().join("client/browser-link/id"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}
