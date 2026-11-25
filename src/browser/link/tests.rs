//! The link without a socket: scripted reads and an injected clock.

use super::*;
use crate::browser::link_proto::{CHUNK_BYTES, MAX_LINE_BYTES};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn facts() -> OpenFacts {
    OpenFacts {
        enabled: true,
        supported: true,
        local_host: false,
        machine: "buildbox".into(),
        now: "14:02".into(),
    }
}

fn params(host_id: &str, label: &str) -> BrowserLinkOpenParams {
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
        .open(&params("mac-a", "mbp"), &facts())
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
    let first = slot.open(&params("mac-a", "mbp"), &facts()).unwrap();
    let second = slot.open(&params("mac-a", "mbp"), &facts()).unwrap();
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
        .open(&params("mac-b", "other"), &facts())
        .expect_err("the second link still holds the slot");
    assert_eq!(err.code, code::LINK_IN_USE);
    slot.close(&second);
    assert_eq!(
        slot.open(&params("mac-b", "other"), &facts())
            .unwrap()
            .epoch,
        3
    );
}

#[test]
fn another_mac_is_told_where_the_browser_is() {
    let slot = LinkSlot::default();
    let live = slot.open(&params("mac-a", "mbp"), &facts()).unwrap();
    let later = OpenFacts {
        now: "15:30".into(),
        ..facts()
    };
    let err = slot.open(&params("mac-b", "other"), &later).unwrap_err();
    assert_eq!(err.code, "link_in_use");
    assert_eq!(
        err.message,
        "the browser of buildbox is on mbp (since 14:02); quit herdr+ there"
    );
    assert_eq!(live.ended(), None);
}

#[test]
fn what_refuses_an_open_changes_nothing() {
    let slot = LinkSlot::default();
    let good = params("mac-a", "mbp");
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
    assert_eq!(refused(&params("  ", "x"), facts()), "invalid_request");
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
    let mut asked = params("mac-a", "m\u{1b}[31m\nbp");
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
        .open(&params("mac-a", &"x".repeat(500)), &facts())
        .unwrap();
    assert_eq!(long.label.len(), LABEL_MAX_CHARS);
    let none = slot.open(&params("mac-a", " "), &facts()).unwrap();
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
    assert!(
        matches!(&waiting(&link)[..], [ServerFrame::BlobAck(ack)] if ack.name == BlobName::Unknown && ack.seq == 9)
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
