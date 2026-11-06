//! Fork (page API): `browser.page_message`. A page an agent showed with
//! `page_api` (`browser show --page-api`) calls `window.herdr.send({text,
//! data})` in the companion; the companion checks the sending frame and
//! forwards it here. The server re-derives everything from its own entry
//! (html_view, page_api, the exact file, opened by an agent pane) and
//! delivers only to that opener, as page content in its own envelope
//! (never "the user", never `browser.send_page`).
//!
//! Delivery is the normal pointer-line path: refused for a target that
//! does not read messages by id (the paste path would land page words in
//! a user paste), never urgent, never a wake-up (an auto-suspended opener
//! is refused), stored quietly under do-not-disturb. A message that
//! arrives while an agent op drives the page (an agent's click or eval)
//! is dropped. Kill switch: `[browser] page_api = false`.

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::{BrowserPageMessageParams, ResponseResult, TabBrowserKind};
use crate::browser::hub;
use crate::coordinator::messages::{AgentMessage, OUTCOME_QUEUED, OUTCOME_SENT};

/// The longest page text (characters).
pub(crate) const MAX_PAGE_TEXT_CHARS: usize = 4_000;
/// The longest page data (bytes of its JSON text).
pub(crate) const MAX_PAGE_DATA_BYTES: usize = 32 * 1024;
/// The deepest page data (nested arrays and objects).
pub(crate) const MAX_PAGE_DATA_DEPTH: usize = 16;
/// Who a page message is from (the log's `from_name`).
pub(crate) const PAGE_MESSAGE_FROM: &str = "browser page";

/// The nesting depth of `value` (a scalar is 0), stopping past `limit`.
fn depth(value: &serde_json::Value, limit: usize) -> usize {
    if limit == 0 {
        return 1;
    }
    match value {
        serde_json::Value::Array(items) => {
            1 + items
                .iter()
                .map(|item| depth(item, limit - 1))
                .max()
                .unwrap_or(0)
        }
        serde_json::Value::Object(map) => {
            1 + map
                .values()
                .map(|item| depth(item, limit - 1))
                .max()
                .unwrap_or(0)
        }
        _ => 0,
    }
}

/// The page data, parsed and re-serialized compact; `Err((code, text))`
/// for bad JSON, too deep, too large.
pub(crate) fn page_data(data_json: Option<&str>) -> Result<Option<String>, (&'static str, String)> {
    let Some(raw) = data_json.filter(|raw| !raw.trim().is_empty()) else {
        return Ok(None);
    };
    if raw.len() > MAX_PAGE_DATA_BYTES {
        return Err((
            "too_large",
            format!("data is {} bytes; at most {MAX_PAGE_DATA_BYTES}", raw.len()),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| ("invalid_data", format!("data is not JSON: {err}")))?;
    if depth(&value, MAX_PAGE_DATA_DEPTH + 1) > MAX_PAGE_DATA_DEPTH {
        return Err((
            "invalid_data",
            format!("data nests deeper than {MAX_PAGE_DATA_DEPTH}"),
        ));
    }
    if value.is_null() {
        return Ok(None);
    }
    serde_json::to_string(&value)
        .map(Some)
        .map_err(|err| ("invalid_data", err.to_string()))
}

impl App {
    /// `browser.page_message`: see the module docs. Every refusal is an
    /// error the companion turns into `{ok: false}` for the page.
    pub(super) fn handle_browser_page_message(
        &mut self,
        id: String,
        params: BrowserPageMessageParams,
    ) -> String {
        let refuse = |id: String, code: &str, text: String, file: &str| {
            tracing::info!(event = "browser.page_message", outcome = code, file = %file, "page message refused");
            encode_error(id, code, text)
        };
        let file = params.file.as_str();
        if !hub().lease_matches(&params.lease_id) {
            let err = crate::browser::hub::lease_unknown();
            return refuse(id, err.code(), err.message().to_string(), file);
        }
        if !hub().config().page_api {
            return refuse(
                id,
                "page_messages_off",
                "page messages are off ([browser] page_api = false)".into(),
                file,
            );
        }
        let chars = params.text.chars().count();
        if chars > MAX_PAGE_TEXT_CHARS {
            return refuse(
                id,
                "too_large",
                format!("text is {chars} characters; at most {MAX_PAGE_TEXT_CHARS}"),
                file,
            );
        }
        let data = match page_data(params.data_json.as_deref()) {
            Ok(data) => data,
            Err((code, text)) => return refuse(id, code, text, file),
        };
        if params.text.trim().is_empty() && data.is_none() {
            return refuse(
                id,
                "invalid_request",
                "a page message needs text or data".into(),
                file,
            );
        }
        // The server's own entry decides, not the host's words.
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return refuse(
                id,
                "tab_not_found",
                format!("no herdr tab {}", params.tab_id),
                file,
            );
        };
        let entry = self.state.workspaces[ws_idx].tabs[tab_idx]
            .browser
            .as_ref()
            .and_then(|browser| {
                browser.entries.iter().find(|entry| {
                    params
                        .entry
                        .as_deref()
                        .is_some_and(|wanted| entry.id == wanted)
                        || params
                            .view_id
                            .as_deref()
                            .is_some_and(|wanted| entry.view_id.as_deref() == Some(wanted))
                })
            })
            .cloned();
        let Some(entry) = entry else {
            return refuse(
                id,
                "not_found",
                format!("herdr tab {} has no such page", params.tab_id),
                file,
            );
        };
        if entry.kind != TabBrowserKind::HtmlView || !entry.page_api || entry.allow_network {
            return refuse(
                id,
                "page_api_off",
                "this page was not shown with page_api".into(),
                file,
            );
        }
        if entry.file.as_deref() != Some(file) {
            return refuse(
                id,
                "wrong_file",
                "the sending page is not the file that was shown".into(),
                file,
            );
        }
        let Some(owner) = entry.opened_by.strip_prefix("agent:").map(str::to_string) else {
            return refuse(
                id,
                "not_agent_opened",
                "only a page an agent showed can message it".into(),
                file,
            );
        };
        let target = match self.resolve_agent_target(&owner) {
            Ok(target)
                if self
                    .public_pane_id(target.ws_idx, target.pane_id)
                    .as_deref()
                    == Some(owner.as_str()) =>
            {
                target
            }
            _ => {
                return refuse(
                    id,
                    "owner_gone",
                    format!("the agent that showed this page ({owner}) is not running"),
                    file,
                )
            }
        };
        // Fail closed: only the exact agent that showed the page (the same
        // server, terminal and agent run); a restarted or replaced agent, a
        // restored page, a page from before this server: owner_gone.
        if entry.opener.as_deref() != Some(hub().opener_token(&target.terminal_id).as_str()) {
            return refuse(
                id,
                "owner_gone",
                format!("the agent that showed this page ({owner}) has restarted or changed; it must show the page again"),
                file,
            );
        }
        if entry
            .target
            .as_deref()
            .is_some_and(|page| hub().agent_driving(page))
        {
            return refuse(
                id,
                "agent_driving",
                "an agent is driving this page; its own clicks send nothing".into(),
                file,
            );
        }
        if self.auto_suspended(&target.terminal_id) {
            return refuse(
                id,
                "agent_suspended",
                format!("{owner} is suspended; a page never wakes it"),
                file,
            );
        }
        if !self.reads_messages(&target.terminal_id) {
            return refuse(
                id,
                "no_message_reader",
                format!(
                    "{owner} does not read herdr messages by id; page messages are never pasted"
                ),
                file,
            );
        }
        let to_pane = owner;
        let to_name = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == target.terminal_id)
            .and_then(|terminal| terminal.agent_name.clone())
            .or_else(|| {
                self.pane_info(target.ws_idx, target.pane_id)
                    .and_then(|info| info.agent)
            })
            .unwrap_or_else(|| to_pane.clone());
        let message_id = crate::coordinator::messages::new_id();
        let now = crate::coordinator::now_unix();
        let envelope = crate::agents_model::envelope::page_envelope(
            &message_id,
            &entry.title,
            file,
            &params.text,
            data.as_deref(),
            now,
        );
        let bytes = params.text.len() + data.as_ref().map_or(0, String::len);
        let message = AgentMessage {
            unix: now,
            from_pane: None,
            from_name: Some(PAGE_MESSAGE_FROM.into()),
            to_pane: to_pane.clone(),
            to_name: Some(to_name.clone()),
            text: envelope.clone(),
            outcome: OUTCOME_QUEUED.into(),
            id: Some(message_id.clone()),
            ..AgentMessage::default()
        };
        let pointer = crate::agents_model::envelope::PointerFrom {
            id: message_id.clone(),
            name: "page".into(),
            relation: crate::agents_model::envelope::PointerRelation::Page,
            ..Default::default()
        };
        let audit = |outcome: &str| {
            tracing::info!(
                event = "browser.page_message",
                outcome,
                tab = %params.tab_id,
                entry = %entry.id,
                file = %file,
                pane = %to_pane,
                bytes,
                message_id = %message_id,
                "page message"
            );
        };
        let answer = |outcome: &str, reason: Option<String>| {
            encode_success(
                id.clone(),
                ResponseResult::BrowserPageMessage {
                    outcome: outcome.into(),
                    to_pane: to_pane.clone(),
                    to_name: to_name.clone(),
                    message_id: message_id.clone(),
                    reason,
                },
            )
        };
        // Do-not-disturb: stored quietly, released as a pointer line.
        if self.pane_in_dnd(target.ws_idx, target.pane_id) {
            let release = super::quiet_messages::QuietRelease {
                envelope: envelope.clone(),
                pointer: Some(pointer),
                from_terminal: None,
                from_coordinator: false,
            };
            return match self.store_quiet_message(&target, message, true, Some(release), now) {
                Ok(()) => {
                    audit("queued_dnd");
                    answer("queued", Some(super::quiet_messages::DND_REASON.into()))
                }
                Err((code, text)) => refuse(id.clone(), &code, text, file),
            };
        }
        let check = self.agent_message_check(&target);
        match self.deliver_agent_message(
            &format!("browser-page-message:{message_id}"),
            &target,
            check,
            message.clone(),
            envelope,
            pointer,
            None,
            false,
            now,
        ) {
            Ok(super::message_queue::MessageDelivery::Sent) => {
                self.log_page_message(AgentMessage {
                    outcome: OUTCOME_SENT.into(),
                    ..message
                });
                audit("sent");
                answer("sent", None)
            }
            Ok(super::message_queue::MessageDelivery::Queued { reason }) => {
                audit("queued");
                answer("queued", Some(reason))
            }
            Err(refused) => refuse(id.clone(), &refused.code, refused.message, file),
        }
    }

    /// The message log line of a page message typed in now (queued ones
    /// the queue logs).
    fn log_page_message(&self, message: AgentMessage) {
        let Some(dir) = self.agents_model.dir.clone() else {
            return;
        };
        if let Err(err) = crate::coordinator::messages::append(&dir, &message) {
            tracing::warn!(err = %err, "browser page_message: cannot append to the message log");
        }
    }
}

#[cfg(test)]
mod tests;
