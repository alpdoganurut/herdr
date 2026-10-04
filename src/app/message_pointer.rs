//! Agent messages as a pointer line (fork). Claude Code tags a bracketed
//! paste as content the user pasted, so an envelope typed in as a paste reads
//! as an un-commented user paste and the agent asks its user before acting.
//! A target whose herdr_agents server reads messages by id gets one short
//! line typed as plain keystrokes instead
//! (`crate::agents_model::envelope::pointer_line`), and reads the envelopes
//! with `agents_messages id=…` (`agents.read_messages`).
//!
//! Which targets: the herdr_agents server says so on `agents.actor`
//! (`reads_messages`, at its `initialize` and on every tool call), and the
//! server records the calling process for the caller's terminal. A message
//! goes in as a pointer while one of those processes still runs under the
//! terminal's process (an agent that exited, or a plain `claude` started
//! later in the same pane, gets the paste again). Everything else keeps the
//! paste: agents without the tools, older herdr_agents servers.
//!
//! The envelopes of messages typed in as a pointer are kept with the queue
//! (`message_queue.json`, so they survive restarts and live handoffs), at
//! most [`DELIVERED_MAX`] for [`DELIVERED_KEEP_S`]; only the agent they were
//! typed into reads them, and reading marks them read. Delivery itself
//! (typing guard, queue, order, claims, coordinator turns) is
//! src/app/message_queue.rs, unchanged.

use serde::{Deserialize, Serialize};

use super::agents_model::{ModelError, ModelResult};
use super::App;
use crate::agents_model::envelope::{pointer_is_safe, pointer_line, PointerFrom};
use crate::api::schema::agents_model::{
    error_code, AgentsDeliveredMessage, AgentsReadMessagesParams,
};
use crate::api::schema::ResponseResult;

/// How many delivered envelopes are kept.
pub(crate) const DELIVERED_MAX: usize = 256;
/// How long a delivered envelope is kept (seconds).
pub(crate) const DELIVERED_KEEP_S: u64 = 24 * 3600;
/// How many server processes are remembered per terminal.
const READERS_PER_TERMINAL: usize = 4;
/// The most ids one `agents.read_messages` reads.
const READ_MAX_IDS: usize = 16;

/// One message on its way into a target: its id, its envelope (the paste)
/// and its pointer facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outgoing {
    pub(crate) id: String,
    pub(crate) envelope: String,
    pub(crate) pointer: PointerFrom,
}

/// A message typed in as a pointer: its envelope, for
/// `agents.read_messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeliveredMessage {
    pub(crate) id: String,
    /// The target's terminal when it was typed in.
    pub(crate) terminal_id: String,
    /// The target agent's native session (the agent restarted in another
    /// terminal still reads it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    pub(crate) envelope: String,
    pub(crate) unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) read_unix: Option<u64>,
}

/// What [`App::message_input`] types into a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageInput {
    pub(crate) text: String,
    /// Plain keystrokes (a pointer line), not a bracketed paste.
    pub(crate) typed: bool,
}

/// Drop what is past its age and over the cap, oldest first.
pub(crate) fn prune_delivered(delivered: &mut Vec<DeliveredMessage>, now_unix: u64) {
    delivered.retain(|message| message.unix + DELIVERED_KEEP_S > now_unix);
    if delivered.len() > DELIVERED_MAX {
        let excess = delivered.len() - DELIVERED_MAX;
        delivered.drain(..excess);
    }
}

impl App {
    /// Record that the herdr_agents server `pid` reads messages for the
    /// agent in `terminal_id`.
    pub(crate) fn note_message_reader(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        pid: u32,
    ) {
        let readers = self
            .agents_model
            .message_readers
            .entry(terminal_id.to_string())
            .or_default();
        if readers.contains(&pid) {
            return;
        }
        readers.push(pid);
        if readers.len() > READERS_PER_TERMINAL {
            readers.remove(0);
        }
    }

    /// Whether a message for `terminal_id` goes in as a pointer line: a
    /// recorded reader still runs under the terminal's process.
    pub(crate) fn reads_messages(&self, terminal_id: &str) -> bool {
        let Some(readers) = self.agents_model.message_readers.get(terminal_id) else {
            return false;
        };
        #[cfg(test)]
        if self.agents_model.readers_unchecked {
            return !readers.is_empty();
        }
        let Some(child) = self
            .state
            .terminals
            .keys()
            .find(|id| id.as_str() == terminal_id)
            .and_then(|id| self.terminal_runtimes.get(id))
            .and_then(|runtime| runtime.child_pid())
        else {
            return false;
        };
        readers
            .iter()
            .any(|pid| crate::platform::process_ancestry(*pid).contains(&child))
    }

    /// What goes into `terminal_id` for `items` (one message, or a queued
    /// batch): one pointer line typed as keys when the target reads
    /// messages by id, else the envelopes as one paste.
    pub(crate) fn message_input(&self, terminal_id: &str, items: &[Outgoing]) -> MessageInput {
        if self.reads_messages(terminal_id) {
            let pointers: Vec<PointerFrom> =
                items.iter().map(|item| item.pointer.clone()).collect();
            let line = pointer_line(&pointers);
            if pointer_is_safe(&line) {
                return MessageInput {
                    text: line,
                    typed: true,
                };
            }
            tracing::warn!(
                line,
                "message pointer: unsafe pointer line, pasting instead"
            );
        }
        let text = items
            .iter()
            .map(|item| item.envelope.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        MessageInput { text, typed: false }
    }

    /// Keep the envelopes of `items`, just typed into `terminal_id` as a
    /// pointer line.
    pub(crate) fn remember_delivered(&mut self, terminal_id: &str, items: &[Outgoing]) {
        let now = crate::coordinator::now_unix();
        let session = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.as_str() == terminal_id)
            .and_then(super::coordinator::terminal_session);
        let delivered = &mut self.message_queue.delivered;
        delivered.retain(|message| !items.iter().any(|item| item.id == message.id));
        delivered.extend(items.iter().map(|item| DeliveredMessage {
            id: item.id.clone(),
            terminal_id: terminal_id.to_string(),
            session: session.clone(),
            envelope: item.envelope.clone(),
            unix: now,
            read_unix: None,
        }));
        prune_delivered(delivered, now);
        self.message_queue.save();
    }

    /// `agents.read_messages`.
    pub(super) fn handle_agents_read_messages(
        &mut self,
        id: String,
        params: AgentsReadMessagesParams,
    ) -> String {
        let result = self.agents_read_messages(&params);
        Self::model_reply(
            id,
            result.map(|messages| ResponseResult::AgentsReadMessages { messages }),
        )
    }

    fn agents_read_messages(
        &mut self,
        params: &AgentsReadMessagesParams,
    ) -> ModelResult<Vec<AgentsDeliveredMessage>> {
        let caller = self.required_caller(&params.caller_pane)?;
        let mut ids: Vec<&str> = Vec::new();
        for id in params.ids.iter().map(|id| id.trim()) {
            if !crate::coordinator::messages::is_id(id) {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    format!("{id:?} is not a message id"),
                ));
            }
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() || ids.len() > READ_MAX_IDS {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                format!("ids: 1 to {READ_MAX_IDS} message ids"),
            ));
        }
        let terminal_id = caller.terminal_id.to_string();
        let session = self
            .state
            .terminals
            .get(&caller.terminal_id)
            .and_then(super::coordinator::terminal_session);
        let now = crate::coordinator::now_unix();
        let mut read_any = false;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let found = self.message_queue.delivered.iter_mut().find(|message| {
                message.id == id
                    && (message.terminal_id == terminal_id
                        || (session.is_some() && message.session == session))
            });
            out.push(match found {
                Some(message) => {
                    let first_read = message.read_unix.is_none();
                    if first_read {
                        message.read_unix = Some(now);
                        read_any = true;
                    }
                    AgentsDeliveredMessage {
                        id: id.to_string(),
                        found: true,
                        text: Some(message.envelope.clone()),
                        delivered_unix: Some(message.unix),
                        first_read,
                    }
                }
                None => AgentsDeliveredMessage {
                    id: id.to_string(),
                    ..AgentsDeliveredMessage::default()
                },
            });
        }
        if read_any {
            self.message_queue.save();
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
