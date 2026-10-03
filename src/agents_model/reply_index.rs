//! The server's reply index: message id → (sender, target, time) for the
//! last day, seeded from the tail of `messages.jsonl` on start. The reply
//! exemption and the coordinator's reply rule read it instead of a window of
//! log lines.

use std::collections::{HashMap, VecDeque};

/// Entries older than this are dropped.
pub const REPLY_WINDOW_S: u64 = 24 * 60 * 60;
/// The most entries kept (a backstop; a loop is stopped long before).
const MAX_ENTRIES: usize = 50_000;

/// One delivered (or logged) message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyEntry {
    /// The sender's public pane id at the time.
    pub from: String,
    /// The target's public pane id at the time.
    pub to: String,
    pub unix: u64,
}

#[derive(Debug, Default)]
pub struct ReplyIndex {
    entries: HashMap<String, ReplyEntry>,
    order: VecDeque<(u64, String)>,
}

impl ReplyIndex {
    /// Seed from the message log (oldest first); lines without an id or a
    /// sender are skipped.
    pub fn seeded<'a>(
        messages: impl IntoIterator<Item = &'a crate::coordinator::messages::AgentMessage>,
        now: u64,
    ) -> Self {
        let mut index = Self::default();
        for message in messages {
            let (Some(id), Some(from)) = (&message.id, &message.from_pane) else {
                continue;
            };
            index.insert(
                id.clone(),
                from.clone(),
                message.to_pane.clone(),
                message.unix,
                now,
            );
        }
        index
    }

    pub fn insert(&mut self, id: String, from: String, to: String, unix: u64, now: u64) {
        self.prune(now);
        if self.entries.len() >= MAX_ENTRIES {
            if let Some((_, oldest)) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back((unix, id.clone()));
        self.entries.insert(id, ReplyEntry { from, to, unix });
    }

    #[cfg(test)]
    pub fn get(&self, id: &str) -> Option<&ReplyEntry> {
        self.entries.get(id)
    }

    /// Whether a message from `from` to `to` answering `reply_to` is a reply
    /// to a message `to` sent to `from`.
    pub fn is_reply(&self, reply_to: Option<&str>, from: &str, to: &str) -> bool {
        reply_to
            .and_then(|id| self.entries.get(id))
            .is_some_and(|entry| entry.from == to && entry.to == from)
    }

    fn prune(&mut self, now: u64) {
        while let Some((unix, _)) = self.order.front() {
            if now.saturating_sub(*unix) <= REPLY_WINDOW_S {
                break;
            }
            if let Some((_, id)) = self.order.pop_front() {
                self.entries.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::messages::AgentMessage;

    #[test]
    fn a_reply_is_recognized_regardless_of_how_old_in_lines_it_is() {
        let mut log = Vec::new();
        log.push(AgentMessage {
            unix: 100,
            from_pane: Some("w1:p1".into()),
            to_pane: "w2:p1".into(),
            id: Some("m1".into()),
            ..AgentMessage::default()
        });
        for i in 0..500 {
            log.push(AgentMessage {
                unix: 101,
                from_pane: Some("w3:p1".into()),
                to_pane: "w4:p1".into(),
                id: Some(format!("x{i}")),
                ..AgentMessage::default()
            });
        }
        let index = ReplyIndex::seeded(&log, 200);
        assert!(index.is_reply(Some("m1"), "w2:p1", "w1:p1"));
        assert!(!index.is_reply(Some("m1"), "w1:p1", "w2:p1"));
        assert!(!index.is_reply(None, "w2:p1", "w1:p1"));
        assert!(!index.is_reply(Some("nope"), "w2:p1", "w1:p1"));
    }

    #[test]
    fn entries_expire_after_a_day() {
        let mut index = ReplyIndex::default();
        index.insert("m1".into(), "a".into(), "b".into(), 10, 10);
        index.insert(
            "m2".into(),
            "a".into(),
            "b".into(),
            10 + REPLY_WINDOW_S + 5,
            10 + REPLY_WINDOW_S + 5,
        );
        assert!(index.get("m1").is_none());
        assert!(index.get("m2").is_some());
    }
}
