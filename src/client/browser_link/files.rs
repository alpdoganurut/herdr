//! The bulk lane of the helper's link: screenshots out as `blob` chunks and
//! shown files in as `file` pulls. Pure: what goes on the link next, and whose
//! answer came, decided without I/O.
//!
//! Both are paced so that a large picture or file never stands between the
//! terminal's keys and the remote machine (amendment A5.3): a few chunks of
//! 64 KiB unacknowledged, a few pulls unanswered.

use std::collections::{HashMap, HashSet, VecDeque};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::Value;

use crate::browser::link_proto::{
    is_file_token, BlobAck, BlobChunk, BlobName, FileReply, FileRequest, LinkReply, BLOB_MAX_BYTES,
    BLOB_WINDOW, CHUNK_BYTES, FILE_MAX_BYTES, FILE_PULLS, FILE_WINDOW,
};

/// A picture of an op's answer, decoded.
pub(crate) type Picture = (BlobName, Vec<u8>);

/// Takes the pictures out of a screenshot's answer (`result.full`,
/// `result.inline`, base64 from the sidecar): what is left is the `re`'s
/// result. An answer without them has none.
pub(crate) fn take_pictures(result: &mut Value) -> Result<Vec<Picture>, String> {
    let Some(map) = result.as_object_mut() else {
        return Ok(Vec::new());
    };
    let mut pictures = Vec::new();
    for (key, name) in [("full", BlobName::Full), ("inline", BlobName::Inline)] {
        match map.remove(key) {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => {
                let data = STANDARD
                    .decode(text.as_bytes())
                    .map_err(|err| format!("the screenshot's {key} picture is no base64: {err}"))?;
                if data.len() as u64 > BLOB_MAX_BYTES {
                    return Err(format!(
                        "the screenshot is {} bytes; at most {BLOB_MAX_BYTES} go over the link",
                        data.len()
                    ));
                }
                pictures.push((name, data));
            }
            Some(_) => return Err(format!("the screenshot's {key} picture is no text")),
        }
    }
    Ok(pictures)
}

struct BlobJob {
    op: u64,
    pictures: Vec<Picture>,
    /// The picture and the offset in it the next chunk starts at.
    picture: usize,
    offset: usize,
    seq: u64,
    sent_all: bool,
    reply: LinkReply,
}

impl BlobJob {
    fn next_chunk(&mut self) -> Option<BlobChunk> {
        if self.sent_all {
            return None;
        }
        let (name, data) = self.pictures.get(self.picture)?;
        let end = (self.offset + CHUNK_BYTES).min(data.len());
        let chunk = BlobChunk {
            op: self.op,
            name: *name,
            seq: self.seq,
            data: data[self.offset..end].to_vec(),
            last: end == data.len(),
            size: data.len() as u64,
        };
        if chunk.last {
            self.picture += 1;
            self.offset = 0;
            self.seq = 0;
            self.sent_all = self.picture == self.pictures.len();
        } else {
            self.offset = end;
            self.seq += 1;
        }
        Some(chunk)
    }
}

/// The pictures under way and the answers that wait for them.
#[derive(Default)]
pub(crate) struct Blobs {
    jobs: VecDeque<BlobJob>,
    /// The chunks on the link, by (op, name, seq).
    unacked: HashSet<(u64, BlobName, u64)>,
}

impl Blobs {
    /// An answer and its pictures. Without pictures the answer is due now.
    pub(crate) fn push(&mut self, reply: LinkReply, pictures: Vec<Picture>) -> Option<LinkReply> {
        if pictures.is_empty() {
            return Some(reply);
        }
        self.jobs.push_back(BlobJob {
            op: reply.id,
            pictures,
            picture: 0,
            offset: 0,
            seq: 0,
            sent_all: false,
            reply,
        });
        None
    }

    /// The next chunk the window lets out, oldest op first.
    pub(crate) fn next_chunk(&mut self) -> Option<BlobChunk> {
        if self.unacked.len() as u64 >= BLOB_WINDOW {
            return None;
        }
        let chunk = self.jobs.iter_mut().find_map(BlobJob::next_chunk)?;
        self.unacked.insert((chunk.op, chunk.name, chunk.seq));
        Some(chunk)
    }

    /// The answers due after this ack: an op's `re` goes after its last
    /// chunk's ack. `None` for an ack of nothing sent.
    pub(crate) fn ack(&mut self, ack: &BlobAck) -> Option<Vec<LinkReply>> {
        if !self.unacked.remove(&(ack.op, ack.name, ack.seq)) {
            return None;
        }
        let mut due = Vec::new();
        while let Some(at) = self
            .jobs
            .iter()
            .position(|job| job.sent_all && !self.unacked.iter().any(|(op, _, _)| *op == job.op))
        {
            if let Some(job) = self.jobs.remove(at) {
                due.push(job.reply);
            }
        }
        Some(due)
    }

    /// The ops whose answers wait here, oldest first.
    pub(crate) fn ops(&self) -> Vec<u64> {
        self.jobs.iter().map(|job| job.op).collect()
    }

    /// Whether an op's answer still waits here.
    #[cfg(test)]
    pub(crate) fn holds(&self, op: u64) -> bool {
        self.jobs.iter().any(|job| job.op == op)
    }

    /// The op's pictures go no further (its error `re` takes their place).
    pub(crate) fn forget(&mut self, op: u64) {
        self.jobs.retain(|job| job.op != op);
        self.unacked.retain(|(id, _, _)| *id != op);
    }

    /// The link ended: nothing of it is sent any more.
    pub(crate) fn clear(&mut self) {
        self.jobs.clear();
        self.unacked.clear();
    }

    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.unacked.len()
    }
}

/// A `link.file` call of the companion: the next piece of a shown file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pull {
    pub(crate) token: String,
    pub(crate) path: String,
    pub(crate) offset: u64,
}

impl Pull {
    /// The companion's `{token, path, offset}`, checked: a token of B's kind,
    /// a relative path, an offset inside the largest file.
    pub(crate) fn parse(params: &Value) -> Result<Self, String> {
        let token = params
            .get("token")
            .and_then(Value::as_str)
            .filter(|token| is_file_token(token))
            .ok_or("link.file needs a file token")?;
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.starts_with('/') && !path.chars().any(char::is_control))
            .ok_or("link.file needs a relative, percent-encoded path")?;
        let offset = params
            .get("offset")
            .map_or(Some(0), Value::as_u64)
            .filter(|offset| *offset <= FILE_MAX_BYTES)
            .ok_or("link.file needs an offset inside the file")?;
        Ok(Self {
            token: token.to_owned(),
            path: path.to_owned(),
            offset,
        })
    }
}

/// The companion's pulls: at most [`FILE_PULLS`] held, [`FILE_WINDOW`] of them
/// on the link. Ids keep rising across links, so an old link's answer never
/// matches a new pull.
#[derive(Default)]
pub(crate) struct FilePulls {
    next_id: u64,
    next_ticket: u64,
    waiting: VecDeque<(u64, Pull)>,
    /// Frame id → ticket.
    on_link: HashMap<u64, u64>,
}

impl FilePulls {
    /// A ticket for the pull, or `None` when as many are held as may be.
    pub(crate) fn ask(&mut self, pull: Pull) -> Option<u64> {
        if self.held() >= FILE_PULLS {
            return None;
        }
        self.next_ticket += 1;
        self.waiting.push_back((self.next_ticket, pull));
        Some(self.next_ticket)
    }

    /// The next `file` frame the window lets out.
    pub(crate) fn next_frame(&mut self) -> Option<FileRequest> {
        if self.on_link.len() >= FILE_WINDOW {
            return None;
        }
        let (ticket, pull) = self.waiting.pop_front()?;
        self.next_id += 1;
        self.on_link.insert(self.next_id, ticket);
        Some(FileRequest {
            id: self.next_id,
            token: pull.token,
            path: pull.path,
            offset: pull.offset,
        })
    }

    /// The ticket a `file_re` answers; `None` for an answer to nothing asked.
    pub(crate) fn answered(&mut self, reply: &FileReply) -> Option<u64> {
        self.on_link.remove(&reply.id)
    }

    /// A waiting pull whose caller gave up.
    pub(crate) fn cancel(&mut self, ticket: u64) {
        self.waiting.retain(|(t, _)| *t != ticket);
    }

    /// The link is down: every held pull ends.
    pub(crate) fn drain(&mut self) -> Vec<u64> {
        let mut tickets: Vec<u64> = self.waiting.drain(..).map(|(t, _)| t).collect();
        tickets.extend(self.on_link.drain().map(|(_, t)| t));
        tickets
    }

    pub(crate) fn held(&self) -> usize {
        self.waiting.len() + self.on_link.len()
    }

    #[cfg(test)]
    pub(crate) fn on_link(&self) -> usize {
        self.on_link.len()
    }
}

/// A `file_re` as the companion's `link.file` answer.
pub(crate) fn file_answer(reply: &FileReply) -> Value {
    serde_json::json!({
        "type": "link_file",
        "status": reply.status,
        "mime": reply.mime,
        "size": reply.size,
        "data": STANDARD.encode(&reply.data),
        "eof": reply.eof,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(id: u64) -> LinkReply {
        LinkReply {
            id,
            ok: true,
            result: json!({"width": 10}),
            page: None,
            error: None,
        }
    }

    fn ack(chunk: &BlobChunk) -> BlobAck {
        BlobAck {
            op: chunk.op,
            name: chunk.name,
            seq: chunk.seq,
        }
    }

    #[test]
    fn pictures_leave_the_result() {
        let mut result = json!({"full": STANDARD.encode(b"abc"), "inline": null, "width": 3});
        let pictures = take_pictures(&mut result).unwrap();
        assert_eq!(pictures, vec![(BlobName::Full, b"abc".to_vec())]);
        assert_eq!(result, json!({"width": 3}));
        assert!(take_pictures(&mut json!({"full": "%%"})).is_err());
        assert!(take_pictures(&mut json!({"full": 3})).is_err());
        assert!(take_pictures(&mut json!([1])).unwrap().is_empty());
    }

    #[test]
    fn blobs_keep_the_window_and_answer_after_the_last_ack() {
        let mut blobs = Blobs::default();
        let full = vec![7u8; CHUNK_BYTES * 5 + 10];
        let inline = vec![1u8; 20];
        assert!(blobs
            .push(
                reply(9),
                vec![(BlobName::Full, full.clone()), (BlobName::Inline, inline)]
            )
            .is_none());
        assert_eq!(blobs.push(reply(10), Vec::new()), Some(reply(10)));
        let mut sent = Vec::new();
        while let Some(chunk) = blobs.next_chunk() {
            sent.push(chunk);
        }
        assert_eq!(sent.len() as u64, BLOB_WINDOW);
        assert!(sent.iter().all(|c| c.data.len() == CHUNK_BYTES && !c.last));
        assert_eq!(
            sent.iter().map(|c| c.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        let mut got = Vec::new();
        let mut due = Vec::new();
        let mut queue: VecDeque<BlobChunk> = sent.into();
        while let Some(chunk) = queue.pop_front() {
            assert!(blobs.in_flight() as u64 <= BLOB_WINDOW);
            got.push((chunk.name, chunk.seq, chunk.data.len(), chunk.last));
            due.extend(blobs.ack(&ack(&chunk)).unwrap());
            if let Some(next) = blobs.next_chunk() {
                queue.push_back(next);
            }
            if !due.is_empty() {
                assert!(queue.is_empty(), "the answer came before a chunk");
            }
        }
        assert_eq!(due, vec![reply(9)]);
        assert_eq!(got.len(), 7);
        assert_eq!(got[5], (BlobName::Full, 5, 10, true));
        assert_eq!(got[6], (BlobName::Inline, 0, 20, true));
        let total: usize = got
            .iter()
            .filter(|g| g.0 == BlobName::Full)
            .map(|g| g.2)
            .sum();
        assert_eq!(total, full.len());
        assert!(blobs
            .ack(&BlobAck {
                op: 9,
                name: BlobName::Full,
                seq: 0
            })
            .is_none());
    }

    #[test]
    fn a_forgotten_op_frees_its_window() {
        let mut blobs = Blobs::default();
        blobs.push(reply(1), vec![(BlobName::Full, vec![0; CHUNK_BYTES * 8])]);
        while blobs.next_chunk().is_some() {}
        assert_eq!(blobs.in_flight() as u64, BLOB_WINDOW);
        assert!(blobs.holds(1));
        blobs.forget(1);
        assert_eq!(blobs.in_flight(), 0);
        assert!(!blobs.holds(1));
    }

    #[test]
    fn pulls_keep_four_on_the_link_and_sixteen_held() {
        let pull = |offset| Pull {
            token: "a".repeat(24),
            path: "index.html".into(),
            offset,
        };
        let mut pulls = FilePulls::default();
        let tickets: Vec<u64> = (0..FILE_PULLS as u64)
            .map(|i| pulls.ask(pull(i)).unwrap())
            .collect();
        assert!(pulls.ask(pull(99)).is_none());
        let mut frames = Vec::new();
        while let Some(frame) = pulls.next_frame() {
            frames.push(frame);
        }
        assert_eq!(frames.len(), FILE_WINDOW);
        let answer = |id| FileReply {
            id,
            status: 200,
            mime: String::new(),
            size: 0,
            data: Vec::new(),
            eof: true,
        };
        assert_eq!(pulls.answered(&answer(frames[0].id)), Some(tickets[0]));
        assert_eq!(pulls.answered(&answer(frames[0].id)), None);
        assert_eq!(pulls.next_frame().map(|f| f.offset), Some(4));
        assert_eq!(pulls.on_link(), FILE_WINDOW);
        assert_eq!(pulls.drain().len(), FILE_PULLS - 1);
        assert_eq!(pulls.held(), 0);
        // ids keep rising after a drain
        pulls.ask(pull(0));
        assert!(pulls.next_frame().unwrap().id > frames[3].id);
    }

    #[test]
    fn a_pull_is_checked() {
        let token = "b".repeat(24);
        assert!(Pull::parse(&json!({"token": token, "path": "a/b.css", "offset": 3})).is_ok());
        assert!(Pull::parse(&json!({"token": "B".repeat(24), "path": "a"})).is_err());
        assert!(Pull::parse(&json!({"token": token, "path": "/etc/passwd"})).is_err());
        assert!(Pull::parse(&json!({"token": token, "path": "a", "offset": -1})).is_err());
        assert!(
            Pull::parse(&json!({"token": token, "path": "a", "offset": FILE_MAX_BYTES + 1}))
                .is_err()
        );
        let answer = file_answer(&FileReply {
            id: 1,
            status: 200,
            mime: "text/css".into(),
            size: 3,
            data: b"a{}".to_vec(),
            eof: true,
        });
        assert_eq!(answer["data"], STANDARD.encode(b"a{}"));
        assert_eq!(answer["size"], 3);
    }
}
