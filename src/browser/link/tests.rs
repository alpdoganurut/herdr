//! The link without a socket: scripted reads and an injected clock.

use super::*;
use crate::browser::link_proto::{CHUNK_BYTES, MAX_LINE_BYTES};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn facts() -> OpenFacts {
    OpenFacts {
        enabled: true,
        supported: true,
        local_host: false,
        machine: "devhost1".into(),
        now: "14:02".into(),
        ..OpenFacts::default()
    }
}

pub(crate) fn mac_params(host_id: &str, label: &str) -> BrowserLinkOpenParams {
    BrowserLinkOpenParams {
        link_protocol: LINK_PROTOCOL,
        host_id: host_id.into(),
        instance: "i1".into(),
        label: label.into(),
        ..Default::default()
    }
}

fn link() -> Arc<Link> {
    LinkSlot::default()
        .open(&mac_params("mac-a", "mac"), &facts())
        .unwrap()
}

/// What waits in the queue, in the writer's order.
fn waiting(link: &Link) -> Vec<ServerFrame> {
    let mut frames = Vec::new();
    while lock(&link.queue.state)
        .lanes
        .iter()
        .any(|lane| !lane.is_empty())
    {
        let line = link.queue.next().unwrap();
        frames.push(ServerFrame::decode(line.as_bytes()).unwrap());
    }
    frames
}

/// The writer's lines after the link ended.
fn last_lines(link: &Link) -> Vec<ServerFrame> {
    std::iter::from_fn(|| link.queue.next())
        .map(|line| ServerFrame::decode(line.as_bytes()).unwrap())
        .collect()
}

// ---- the clock and the bytes of a reading thread

enum Step {
    Bytes(Vec<u8>),
    /// The read times out after this long.
    Idle(Duration),
}

/// A socket whose reads follow a script; its end is the end of the stream.
struct Script {
    steps: VecDeque<Step>,
    clock: Rc<Cell<Instant>>,
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.steps.pop_front() {
            None => Ok(0),
            Some(Step::Idle(waited)) => {
                self.clock.set(self.clock.get() + waited);
                Err(io::ErrorKind::WouldBlock.into())
            }
            Some(Step::Bytes(mut bytes)) => {
                let take = bytes.len().min(buf.len());
                buf[..take].copy_from_slice(&bytes[..take]);
                if take < bytes.len() {
                    self.steps.push_front(Step::Bytes(bytes.split_off(take)));
                }
                Ok(take)
            }
        }
    }
}

fn read_script(link: &Arc<Link>, handler: Arc<dyn LinkHandler>, steps: Vec<Step>) -> LinkEnd {
    let clock = Rc::new(Cell::new(Instant::now()));
    let mut script = Script {
        steps: steps.into(),
        clock: Rc::clone(&clock),
    };
    LinkReader::new(Arc::clone(link), handler, clock.get()).run(
        &mut script,
        || clock.get(),
        || false,
    )
}

fn read_lines(link: &Arc<Link>, lines: &[&str]) -> LinkEnd {
    let bytes = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    read_script(link, Arc::new(Unserved), vec![Step::Bytes(bytes.into())])
}

const PING: &str = r#"{"t":"ping","n":1}"#;

fn lost() -> LinkEnd {
    LinkEnd::Lost("the connection closed".into())
}

// ---- who may open it

#[test]
fn the_same_mac_replaces_its_link_and_the_old_connection_takes_nothing_with_it() {
    let slot = LinkSlot::default();
    let first = slot.open(&mac_params("mac-a", "mac"), &facts()).unwrap();
    let second = slot.open(&mac_params("mac-a", "mac"), &facts()).unwrap();
    assert_eq!((first.epoch, second.epoch), (1, 2));
    assert_ne!(first.link_id, second.link_id);
    assert_eq!(first.ended(), Some(LinkEnd::Replaced));
    assert!(
        matches!(last_lines(&first).as_slice(), [ServerFrame::Bye(bye)] if bye.code == bye::REPLACED)
    );
    assert_eq!(second.ended(), None);
    // the first one's connection ends only now: the second stays
    slot.close(&first);
    let err = slot
        .open(&mac_params("mac-b", "other"), &facts())
        .expect_err("the second link still holds the slot");
    assert_eq!(err.code, code::LINK_IN_USE);
    slot.close(&second);
    assert_eq!(
        slot.open(&mac_params("mac-b", "other"), &facts())
            .unwrap()
            .epoch,
        3
    );
}

#[test]
fn another_mac_is_told_where_the_browser_is() {
    let slot = LinkSlot::default();
    let live = slot.open(&mac_params("mac-a", "mac"), &facts()).unwrap();
    let later = OpenFacts {
        now: "15:30".into(),
        ..facts()
    };
    let err = slot
        .open(&mac_params("mac-b", "other"), &later)
        .unwrap_err();
    assert_eq!(err.code, "link_in_use");
    assert_eq!(
        err.message,
        "the browser of devhost1 is on mac (since 14:02); quit herdr+ there"
    );
    assert_eq!(live.ended(), None);
}

#[test]
fn what_refuses_an_open_changes_nothing() {
    let slot = LinkSlot::default();
    let good = mac_params("mac-a", "mac");
    let refused = |params: &BrowserLinkOpenParams, facts: OpenFacts| {
        slot.open(params, &facts).unwrap_err().code
    };
    let off = OpenFacts {
        enabled: false,
        ..facts()
    };
    assert_eq!(refused(&good, off), "browser_disabled");
    let windows = OpenFacts {
        supported: false,
        ..facts()
    };
    assert_eq!(refused(&good, windows), "link_disabled");
    let newer = BrowserLinkOpenParams {
        link_protocol: LINK_PROTOCOL + 1,
        ..good.clone()
    };
    assert_eq!(refused(&newer, facts()), "link_protocol_unsupported");
    assert_eq!(refused(&mac_params("  ", "x"), facts()), "invalid_request");
    let no_instance = BrowserLinkOpenParams {
        instance: String::new(),
        ..good.clone()
    };
    assert_eq!(refused(&no_instance, facts()), "invalid_request");
    let local = OpenFacts {
        local_host: true,
        ..facts()
    };
    assert_eq!(refused(&good, local), "link_in_use");
    // none of them used an epoch or ended a link
    let live = slot.open(&good, &facts()).unwrap();
    assert_eq!(live.epoch, 1);
    assert_eq!(refused(&newer, facts()), "link_protocol_unsupported");
    assert_eq!(live.ended(), None);
}

#[test]
fn the_answer_names_the_contract_and_a_label_is_one_short_line() {
    let slot = LinkSlot::default();
    let mut asked = mac_params("mac-a", "m\u{1b}[31m\nbp");
    asked.features = vec!["files.reload.v1".into()];
    let link = slot.open(&asked, &facts()).unwrap();
    assert_eq!(link.label, "m[31mbp");
    let info = link.info(&slot.boot_id);
    assert_eq!((info.ping_ms, info.ttl_ms), (3_000, 15_000));
    assert_eq!((info.link_protocol, info.epoch), (1, 1));
    // a feature only the helper has is not on
    assert_eq!(info.features, Vec::<String>::new());
    assert_eq!(info.boot_id.len(), 16);
    let long = slot
        .open(&mac_params("mac-a", &"x".repeat(500)), &facts())
        .unwrap();
    assert_eq!(long.label.len(), LABEL_MAX_CHARS);
    let none = slot.open(&mac_params("mac-a", " "), &facts()).unwrap();
    assert_eq!(none.label, NO_LABEL);
}

// ---- the queue

#[test]
fn the_writer_takes_pongs_then_work_then_files() {
    let link = link();
    link.answer(&ServerFrame::FileRe(no_file(7)));
    link.answer(&ServerFrame::HostOk {
        lease_id: "L1".into(),
    });
    link.answer(&ServerFrame::Pong { n: 1 });
    assert_eq!(
        waiting(&link),
        [
            ServerFrame::Pong { n: 1 },
            ServerFrame::HostOk {
                lease_id: "L1".into()
            },
            ServerFrame::FileRe(no_file(7)),
        ]
    );
}

#[test]
fn a_full_queue_still_takes_the_answers_until_the_helper_reads_none() {
    let link = link();
    let line = || "x".repeat(64 * 1024);
    while link
        .queue
        .push(WriteLane::Work, line(), QUEUE_BYTES)
        .is_ok()
    {}
    assert_eq!(
        link.queue.push(WriteLane::Work, line(), QUEUE_BYTES),
        Err(NotQueued::Full)
    );
    assert!(lock(&link.queue.state).bytes <= QUEUE_BYTES);
    // the answers have their own room
    link.answer(&ServerFrame::Pong { n: 1 });
    assert_eq!(link.ended(), None);
    for n in 0..1_000_000 {
        link.answer(&ServerFrame::Pong { n });
        if link.ended().is_some() {
            break;
        }
        assert!(lock(&link.queue.state).bytes <= ANSWER_BYTES);
    }
    assert_eq!(link.ended(), Some(LinkEnd::Backlog));
    assert_eq!(last_lines(&link), []);
}

#[test]
fn nothing_but_the_goodbye_follows_the_end() {
    let link = link();
    link.answer(&ServerFrame::Pong { n: 1 });
    link.end(LinkEnd::Stopping);
    link.end(LinkEnd::Silent);
    link.answer(&ServerFrame::Pong { n: 2 });
    assert_eq!(link.ended(), Some(LinkEnd::Stopping));
    assert!(
        matches!(last_lines(&link).as_slice(), [ServerFrame::Bye(bye)] if bye.code == bye::STOPPING)
    );
}

/// Takes a few bytes a time, after a full socket buffer now and then.
struct SlowSink {
    taken: Vec<u8>,
    calls: usize,
}

impl Write for SlowSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if !self.calls.is_multiple_of(3) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let take = buf.len().min(5);
        self.taken.extend_from_slice(&buf[..take]);
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_slow_socket_never_tears_a_line() {
    let link = link();
    let frames = [
        ServerFrame::Pong { n: 1 },
        ServerFrame::HostOk {
            lease_id: "L".repeat(200),
        },
    ];
    for frame in &frames {
        let line = frame.encode().unwrap();
        let mut sink = SlowSink {
            taken: Vec::new(),
            calls: 0,
        };
        write_line(&mut sink, line.as_bytes(), &link).unwrap();
        assert_eq!(sink.taken, line.as_bytes());
    }
}

#[test]
fn a_stuck_socket_holds_the_writer_only_for_the_grace() {
    struct Stuck;
    impl Write for Stuck {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(10));
            Err(io::ErrorKind::WouldBlock.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let link = link();
    link.end(LinkEnd::Stopping);
    let started = Instant::now();
    write_until_end(&mut Stuck, &link);
    assert!(started.elapsed() >= BYE_GRACE);
    assert!(started.elapsed() < BYE_GRACE * 4);
}

// ---- reading

#[test]
fn fifteen_silent_seconds_end_the_link_and_any_byte_starts_them_again() {
    let idle = |ms| Step::Idle(Duration::from_millis(ms));
    let silent = |steps| read_script(&link(), Arc::new(Unserved), steps);
    assert_eq!(silent(vec![idle(14_999)]), lost());
    assert_eq!(silent(vec![idle(14_999), idle(1)]), LinkEnd::Silent);
    assert_eq!(
        silent(vec![
            idle(14_999),
            Step::Bytes(b"{\"t\":".to_vec()),
            idle(14_999),
            Step::Bytes(b"\"ping\",\"n\":1}\n".to_vec()),
            idle(14_999),
        ]),
        lost()
    );
    assert_eq!(LinkEnd::Silent.bye(), None);
}

#[test]
fn an_unknown_frame_is_ignored_and_a_broken_known_one_drops_the_link() {
    let kept = link();
    let end = read_lines(
        &kept,
        &[
            r#"{"t":"pong_please","n":{"x":1}}"#,
            r#"{"t":"ping","n":4,"new":1}"#,
        ],
    );
    assert_eq!(end, lost());
    assert_eq!(waiting(&kept), [ServerFrame::Pong { n: 4 }]);
    for bad in [
        r#"{"t":"ping","n":"four"}"#,
        r#"{"t":"ping"}"#,
        r#"{"n":1}"#,
        r#"["ping",1]"#,
        "ping",
        r#"{"t":"blob","op":1,"name":"full","seq":0,"data":"!!","size":1}"#,
    ] {
        let dropped = link();
        let end = read_lines(&dropped, &[bad, PING]);
        assert!(matches!(end, LinkEnd::Protocol(_)), "{bad}: {end:?}");
        assert_eq!(end.bye().unwrap().code, "protocol");
        // what follows it is not read
        assert_eq!(waiting(&dropped), [], "{bad}");
    }
}

#[test]
fn a_line_over_the_cap_drops_the_link_before_its_end_arrives() {
    let long = "x".repeat(MAX_LINE_BYTES + 1);
    let end = read_script(
        &link(),
        Arc::new(Unserved),
        vec![
            Step::Bytes(long.into_bytes()),
            Step::Idle(Duration::from_millis(1)),
        ],
    );
    assert!(matches!(end, LinkEnd::Protocol(_)), "{end:?}");
    // one that fits is read
    let pad = "x".repeat(MAX_LINE_BYTES - 40);
    let fits = format!(r#"{{"t":"ping","n":9,"pad":"{pad}"}}"#);
    let kept = link();
    assert_eq!(read_lines(&kept, &[&fits]), lost());
    assert_eq!(waiting(&kept), [ServerFrame::Pong { n: 9 }]);
}

#[test]
fn the_helpers_goodbye_ends_the_link_and_gets_none_back() {
    let link = link();
    let end = read_lines(&link, &[r#"{"t":"bye","code":"quit"}"#, PING]);
    assert!(matches!(&end, LinkEnd::Bye(bye) if bye.code == bye::QUIT));
    assert_eq!(end.bye(), None);
    assert_eq!(waiting(&link), []);
}

#[test]
fn a_server_that_stops_says_so() {
    let link = link();
    let clock = Rc::new(Cell::new(Instant::now()));
    let mut script = Script {
        steps: vec![Step::Idle(Duration::from_millis(1))].into(),
        clock: Rc::clone(&clock),
    };
    let end = LinkReader::new(Arc::clone(&link), Arc::new(Unserved), clock.get()).run(
        &mut script,
        || clock.get(),
        || true,
    );
    assert_eq!(end, LinkEnd::Stopping);
    assert_eq!(end.bye().unwrap().code, "stopping");
}

/// Counts what reaches it.
#[derive(Default)]
struct Counter {
    events: AtomicUsize,
    blobs: AtomicUsize,
    files: AtomicUsize,
}

impl LinkHandler for Counter {
    fn event(&self, link: &Arc<Link>, _event: serde_json::Value) {
        // a newer link takes over in the middle of what was read
        if self.events.fetch_add(1, Ordering::SeqCst) == 1 {
            link.end(LinkEnd::Replaced);
        }
    }
    fn blob(&self, _link: &Arc<Link>, _chunk: &BlobChunk) {
        self.blobs.fetch_add(1, Ordering::SeqCst);
    }
    fn file(&self, _link: &Arc<Link>, request: &FileRequest) -> FileReply {
        self.files.fetch_add(1, Ordering::SeqCst);
        FileReply {
            // the link puts the request's own id in
            id: 0,
            data: vec![1; CHUNK_BYTES],
            ..no_file(request.id)
        }
    }
}

#[test]
fn a_replaced_links_frames_do_nothing() {
    let counter = Arc::new(Counter::default());
    let event = "{\"t\":\"ev\",\"event\":{}}\n".repeat(5);
    let end = read_script(&link(), counter.clone(), vec![Step::Bytes(event.into())]);
    assert_eq!(end, LinkEnd::Replaced);
    assert_eq!(counter.events.load(Ordering::SeqCst), 2);
}

#[test]
fn an_api_call_nobody_serves_is_refused() {
    let link = link();
    read_lines(
        &link,
        &[r#"{"t":"api","id":3,"method":"pane.send_text","params":{}}"#],
    );
    let [ServerFrame::ApiRe(reply)] = &waiting(&link)[..] else {
        panic!("one api_re");
    };
    assert_eq!((reply.id, reply.ok), (3, false));
    assert_eq!(reply.error.as_ref().unwrap().code, "invalid_request");
}

// ---- pictures and files

fn chunk(op: u64, seq: u64, bytes: usize, last: bool, size: u64) -> BlobChunk {
    BlobChunk {
        op,
        name: BlobName::Full,
        seq,
        data: vec![7; bytes],
        last,
        size,
    }
}

#[test]
fn each_chunk_in_its_order_is_acknowledged() {
    let link = link();
    let counter = Arc::new(Counter::default());
    let size = CHUNK_BYTES as u64 + 3;
    let lines: String = [
        chunk(5, 0, CHUNK_BYTES, false, size),
        chunk(5, 1, 3, true, size),
    ]
    .into_iter()
    .map(|chunk| HelperFrame::Blob(chunk).encode().unwrap())
    .collect();
    assert_eq!(
        read_script(&link, counter.clone(), vec![Step::Bytes(lines.into())]),
        lost()
    );
    let acks: Vec<u64> = waiting(&link)
        .into_iter()
        .map(|frame| match frame {
            ServerFrame::BlobAck(ack) => {
                assert_eq!((ack.op, ack.name), (5, BlobName::Full));
                ack.seq
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(acks, [0, 1]);
    assert_eq!(counter.blobs.load(Ordering::SeqCst), 2);
    // a newer helper's picture is acknowledged and reaches nobody
    let newer = r#"{"t":"blob","op":5,"name":"thumbnail","seq":9,"data":"","size":0}"#;
    read_lines(&link, &[newer]);
    // with its own name, as it came
    let ack: serde_json::Value = serde_json::from_str(&link.queue.next().unwrap()).unwrap();
    assert_eq!(
        ack,
        serde_json::json!({ "t": "blob_ack", "op": 5, "name": "thumbnail", "seq": 9 })
    );
    assert_eq!(counter.blobs.load(Ordering::SeqCst), 2);
}

#[test]
fn a_chunk_that_does_not_fit_its_blob_is_refused() {
    let first = chunk(1, 0, 10, false, 20);
    let after_first = |next: BlobChunk| {
        let mut blobs = Blobs::default();
        blobs.check(&first).unwrap();
        blobs.check(&next)
    };
    assert_eq!(after_first(chunk(1, 1, 10, true, 20)), Ok(()));
    for bad in [
        chunk(1, 0, 10, false, 20),
        chunk(1, 2, 10, true, 20),
        chunk(1, 1, 11, true, 20),
        chunk(1, 1, 9, true, 20),
        chunk(1, 1, 10, true, 21),
    ] {
        assert!(
            after_first(bad.clone()).is_err(),
            "{:?}",
            (bad.seq, bad.size)
        );
    }
    let mut blobs = Blobs::default();
    assert!(blobs.check(&chunk(1, 1, 1, false, 2)).is_err());
    assert!(blobs
        .check(&chunk(1, 0, 1, false, BLOB_MAX_BYTES + 1))
        .is_err());
    assert_eq!(blobs.check(&chunk(1, 0, 0, true, 0)), Ok(()));
    // the answer ends what an op left unfinished; the same id starts anew
    for op in 0..BLOBS_OPEN as u64 {
        blobs.check(&chunk(op, 0, 1, false, 2)).unwrap();
    }
    assert!(blobs.check(&chunk(999, 0, 1, false, 2)).is_err());
    blobs.answered(3);
    assert_eq!(blobs.check(&chunk(999, 0, 1, false, 2)), Ok(()));
}

fn file_line(id: u64) -> String {
    HelperFrame::File(FileRequest {
        id,
        token: "a".repeat(24),
        path: "index.html".into(),
        offset: 0,
    })
    .encode()
    .unwrap()
}

#[test]
fn files_wait_for_the_writer_four_at_a_time() {
    let link = link();
    let counter = Arc::new(Counter::default());
    let lines: String = (1..=10).map(file_line).collect();
    let clock = Instant::now();
    let mut reader = LinkReader::new(Arc::clone(&link), counter.clone(), clock);
    assert_eq!(reader.bytes(lines.as_bytes()), None);
    let bulk = || lock(&link.queue.state).lanes[lane_index(WriteLane::Bulk)].len();
    let until = Instant::now() + Duration::from_secs(5);
    while bulk() < FILE_WINDOW && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    // no file is read before the writer has room for it
    assert_eq!(bulk(), FILE_WINDOW);
    assert_eq!(counter.files.load(Ordering::SeqCst), FILE_WINDOW);
    let ids: Vec<u64> = (1..=10)
        .map(|_| {
            let line = link.queue.next().unwrap();
            match ServerFrame::decode(line.as_bytes()).unwrap() {
                ServerFrame::FileRe(reply) => reply.id,
                other => panic!("{other:?}"),
            }
        })
        .collect();
    assert_eq!(ids, (1..=10).collect::<Vec<_>>());
}

#[test]
fn more_file_requests_than_a_helper_keeps_drop_the_link() {
    let link = link();
    let counter: Arc<dyn LinkHandler> = Arc::new(Counter::default());
    let mut reader = LinkReader::new(Arc::clone(&link), counter, Instant::now());
    // answered and waiting in the queue, taken by the thread, in its channel
    let most = FILE_WINDOW + 1 + FILE_PULLS;
    let mut end = None;
    for id in 0..=most as u64 {
        end = reader.bytes(file_line(id).as_bytes());
        if end.is_some() {
            break;
        }
    }
    assert!(matches!(end, Some(LinkEnd::Protocol(_))), "{end:?}");
    // its thread ends with the link
    link.end(LinkEnd::Stopping);
    assert!(!link.queue.wait_for_room(WriteLane::Bulk, 1));
}

#[test]
fn a_goodbye_names_what_the_helper_said_clipped_to_one_line() {
    let end = LinkEnd::Bye(Bye {
        code: bye::QUIT.into(),
        message: format!(" gone\u{1b}[2J\n{}", "x".repeat(200)),
    });
    let shown = end.to_string();
    assert_eq!(
        shown,
        format!("bye quit: gone[2J{}", "x".repeat(LABEL_MAX_CHARS - 7))
    );
}

/// A whole helper, for the hub's tests (unix: they use the fake sidecar).
#[cfg(unix)]
pub(crate) mod fake_mac {
    use super::*;
    use crate::browser::link_proto::{Ensure, LinkOp};

    /// What [`FakeMac`] got and how it behaves.
    #[derive(Default)]
    pub(crate) struct MacState {
        pub ops: Vec<LinkOp>,
        pub ensures: Vec<Ensure>,
        pub leases: Vec<String>,
        pub byes: Vec<String>,
        /// Ops it never answers.
        pub silent: Vec<String>,
        /// Its answer to `ensure`: its browser comes up, or why not.
        pub ensure_error: Option<LinkError>,
        /// It never answers an `ensure`.
        pub ensure_silent: bool,
        pub instance: String,
        /// The pages its companion has (`host_up`).
        pub tabs: Vec<String>,
        /// Its screenshots: the full picture's bytes, sent in 3 chunks
        /// (none: only the small one comes), and whether it stops after
        /// the first chunk.
        pub picture: Option<Vec<u8>>,
        pub picture_cut: bool,
        /// Its pictures come whole, the `re` never.
        pub picture_unanswered: bool,
        /// Its pictures come whole, then a `re` that failed.
        pub picture_refused: bool,
        /// The answers to its `file` pulls.
        pub files: Vec<FileReply>,
        pub next_target: u32,
    }

    /// The helper of a hub's link on a thread of its own, without a socket.
    pub(crate) struct FakeMac {
        pub link: Arc<Link>,
        pub state: Arc<Mutex<MacState>>,
        reader: Arc<Mutex<LinkReader>>,
        hub: crate::browser::hub::BrowserHub,
    }

    impl FakeMac {
        pub fn open(
            hub: &crate::browser::hub::BrowserHub,
            host_id: &str,
            label: &str,
        ) -> Result<Self, BrowserError> {
            Self::open_with(hub, mac_params(host_id, label)).inspect(|mac| {
                lock(&mac.state).picture = Some(b"the full picture".to_vec());
            })
        }

        pub fn open_with(
            hub: &crate::browser::hub::BrowserHub,
            params: BrowserLinkOpenParams,
        ) -> Result<Self, BrowserError> {
            let link = hub.link_open(&params, facts())?;
            let reader = Arc::new(Mutex::new(LinkReader::new(
                Arc::clone(&link),
                Arc::new(hub.clone()),
                Instant::now(),
            )));
            let state = Arc::new(Mutex::new(MacState {
                instance: "i1".into(),
                ..Default::default()
            }));
            {
                let (link, reader, state) =
                    (Arc::clone(&link), Arc::clone(&reader), Arc::clone(&state));
                std::thread::spawn(move || {
                    while let Some(line) = link.queue.next() {
                        let frame = ServerFrame::decode(line.as_bytes()).unwrap();
                        for answer in heard(&state, frame) {
                            lock(&reader).bytes(answer.encode().unwrap().as_bytes());
                        }
                    }
                });
            }
            Ok(Self {
                link,
                state,
                reader,
                hub: hub.clone(),
            })
        }

        pub fn say(&self, frame: &HelperFrame) -> Option<LinkEnd> {
            lock(&self.reader).bytes(frame.encode().unwrap().as_bytes())
        }

        /// Its companion serves `targets`.
        pub fn up(&self, instance: &str, targets: &[&str]) {
            {
                let mut state = lock(&self.state);
                state.instance = instance.into();
                state.tabs = targets.iter().map(|target| target.to_string()).collect();
            }
            self.say(&HelperFrame::HostUp(up(instance, targets)));
        }

        /// The connection ends, as `serve` ends it.
        pub fn close(&self, end: LinkEnd) {
            self.link.end(end.clone());
            self.hub.link_slot().close(&self.link);
            self.hub.closed(&self.link, &end);
        }

        pub fn op_names(&self) -> Vec<String> {
            lock(&self.state)
                .ops
                .iter()
                .map(|op| op.op.clone())
                .collect()
        }

        /// Wait until it got `op`.
        pub fn got(&self, op: &str) {
            let until = Instant::now() + Duration::from_secs(5);
            while !self.op_names().iter().any(|name| name == op) {
                assert!(Instant::now() < until, "{op} never came");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// A screenshot's answer: its pictures in chunks, then the `re`.
    fn picture(state: &MacState, id: u64, url: &str) -> Vec<HelperFrame> {
        let chunks = |name: BlobName, bytes: &[u8], parts: usize, cut: bool| {
            let size = bytes.len() as u64;
            let step = bytes.len().div_ceil(parts).max(1);
            let pieces: Vec<&[u8]> = if bytes.is_empty() {
                vec![&[][..]]
            } else {
                bytes.chunks(step).collect()
            };
            let count = pieces.len();
            pieces
                .into_iter()
                .enumerate()
                .take(if cut { 1 } else { count })
                .map(|(seq, data)| {
                    HelperFrame::Blob(BlobChunk {
                        op: id,
                        name,
                        seq: seq as u64,
                        data: data.to_vec(),
                        last: seq + 1 == count,
                        size,
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut frames = Vec::new();
        if let Some(full) = &state.picture {
            frames.extend(chunks(BlobName::Full, full, 3, state.picture_cut));
        }
        if state.picture_cut {
            return frames;
        }
        frames.extend(chunks(BlobName::Inline, b"tiny", 1, false));
        if state.picture_unanswered {
            return frames;
        }
        if state.picture_refused {
            frames.push(HelperFrame::Re(LinkReply {
                id,
                ok: false,
                result: serde_json::Value::Null,
                page: None,
                error: Some(LinkError::new("host_failed", "the page went away")),
            }));
            return frames;
        }
        frames.push(HelperFrame::Re(LinkReply {
            id,
            ok: true,
            result: serde_json::json!({ "width": 800, "height": 600, "bytes": 16 }),
            page: Some(crate::browser::link_proto::LinkPage {
                url: url.to_string(),
                title: "T".into(),
                ..Default::default()
            }),
            error: None,
        }));
        frames
    }

    fn up(instance: &str, targets: &[&str]) -> HostUp {
        HostUp {
            instance: instance.into(),
            version: "0.2.13".into(),
            electron: "45.0.0".into(),
            tabs: targets
                .iter()
                .map(|target| crate::browser::state::HostTab {
                    target: (*target).into(),
                    url: format!("https://{target}.test/"),
                    title: (*target).into(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    /// What the fake Mac answers.
    fn heard(state: &Mutex<MacState>, frame: ServerFrame) -> Vec<HelperFrame> {
        let mut state = lock(state);
        match frame {
            ServerFrame::Op(op) => {
                let name = op.op.clone();
                let (id, target) = (op.id, op.target.clone());
                let url = op.args["url"]
                    .as_str()
                    .unwrap_or("https://x.test/")
                    .to_string();
                state.ops.push(op);
                if state.silent.contains(&name) {
                    return Vec::new();
                }
                let mut result =
                    serde_json::json!({ "kind": "click", "content": "text", "total_chars": 4 });
                if name == "screenshot" {
                    return picture(&state, id, &url);
                }
                if name == "open" || name == "show_remote" {
                    state.next_target += 1;
                    result["target"] = format!("V{}", state.next_target).into();
                }
                if name == "release" {
                    result = serde_json::json!({ "failed": [] });
                }
                let _ = target;
                vec![HelperFrame::Re(LinkReply {
                    id,
                    ok: true,
                    result,
                    page: Some(crate::browser::link_proto::LinkPage {
                        url,
                        title: "T".into(),
                        ..Default::default()
                    }),
                    error: None,
                })]
            }
            ServerFrame::Ensure(ensure) => {
                let id = ensure.id;
                state.ensures.push(ensure);
                if state.ensure_silent {
                    return Vec::new();
                }
                let error = state.ensure_error.clone();
                let mut answers = Vec::new();
                if error.is_none() {
                    let tabs: Vec<&str> = state.tabs.iter().map(String::as_str).collect();
                    answers.push(HelperFrame::HostUp(up(&state.instance, &tabs)));
                }
                answers.push(HelperFrame::EnsureRe(EnsureReply {
                    id,
                    ok: error.is_none(),
                    error,
                }));
                answers
            }
            ServerFrame::HostOk { lease_id } => {
                state.leases.push(lease_id);
                Vec::new()
            }
            ServerFrame::FileRe(reply) => {
                state.files.push(reply);
                Vec::new()
            }
            ServerFrame::Bye(bye) => {
                state.byes.push(bye.code);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}
