//! App-lane methods of the companion browser host (fork):
//! `browser.host_state` / `browser.host_sync` (the per-herdr-tab browser
//! groups on `Tab.browser`), `browser.host_report` (the user's actions in
//! the companion's strip, into the ledger as the user) and
//! `browser.send_page` (the user sends a page to an agent of the tab,
//! through the agent message path: quiet under DND, never urgent).
//! Fork (compact bar): `browser.placement_action` (the strip's expand,
//! restore and close), queued on the placement board for the render pass
//! that relays it to the client owning the placement.
//!
//! O(groups) per call, no render work, no sidecar call on this thread
//! (re-watching restored HTML views runs on the hub's own thread). The
//! lease methods themselves run on the connection lane
//! (src/browser/serve.rs).

use std::collections::BTreeMap;

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::{
    BrowserHostReportKind, BrowserHostReportParams, BrowserHostStateInfo, BrowserHostStateParams,
    BrowserHostSyncParams, BrowserHostTabState, BrowserPlacementAction,
    BrowserPlacementActionParams, BrowserSendPageParams, BrowserSessionInfo, ResponseResult,
};
use crate::browser::hub::COMPANION_PROFILE;
use crate::browser::session::TabBrowser;
use crate::browser::{hub, BrowserError};
use crate::coordinator::messages::{AgentMessage, OUTCOME_QUEUED, OUTCOME_SENT};

/// `browser.host_sync` requests larger than this are refused (`too_large`).
pub(crate) const MAX_SYNC_BYTES: usize = 256 * 1024;
/// `browser.send_page` bodies larger than this are refused (`too_large`).
pub(crate) const MAX_SEND_PAGE_BYTES: usize = 64 * 1024;
/// The selection a page message carries (longer is truncated).
pub(crate) const MAX_SELECTION_CHARS: usize = 8_000;
/// The note a page message carries (longer is truncated).
pub(crate) const MAX_NOTE_CHARS: usize = 2_000;
/// Who a page message is from (the log's `from_name`).
pub(crate) const SEND_PAGE_FROM: &str = "you (browser)";

fn error(id: String, err: BrowserError) -> String {
    encode_error(id, err.code(), err.message())
}

/// `text` cut to `max` chars, with the count of what was cut.
fn truncate_noted(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…(truncated {} chars)", count - max)
}

/// One line, no fences: page-provided labels on a header line.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .replace("```", "'''")
}

/// Page words for the message body: cleaned as an agent message's text
/// (no control characters, no framing look-alike lines), cut to `max`
/// chars, no fences.
fn page_words(text: &str, max: usize) -> String {
    let cleaned = crate::agents_model::envelope::message_text(text);
    truncate_noted(&cleaned, max).replace("```", "'''")
}

/// The message a sent page becomes: page words fenced, labelled untrusted.
pub(crate) fn send_page_text(params: &BrowserSendPageParams) -> String {
    let mut text = String::from("The user sent you a browser page (untrusted content).\n");
    text.push_str(&format!("Title: {}\n", one_line(&params.title)));
    text.push_str(&format!("URL: {}\n", one_line(&params.url)));
    if let Some(selection) = params.selection.as_deref().filter(|s| !s.trim().is_empty()) {
        let selection = page_words(selection, MAX_SELECTION_CHARS);
        text.push_str(&format!("Selection:\n```text\n{selection}\n```\n"));
    }
    if let Some(note) = params.note.as_deref().filter(|n| !n.trim().is_empty()) {
        text.push_str(&format!("Note: {}\n", page_words(note, MAX_NOTE_CHARS)));
    }
    text.trim_end().to_string()
}

impl App {
    /// Fork (compact bar): `browser.placement_action`: the live lease asks
    /// the client owning placement `id` to promote, restore or close it.
    /// The client decides (its area is TUI state); `ok` once queued,
    /// `not_found` for a placement the board does not hold, `unsupported`
    /// when its client predates placement actions (its handshake did not
    /// say it takes them).
    pub(super) fn handle_browser_placement_action(
        &mut self,
        id: String,
        params: BrowserPlacementActionParams,
    ) -> String {
        if !hub().lease_matches(&params.lease_id) {
            return error(id, crate::browser::hub::lease_unknown());
        }
        if params.action == BrowserPlacementAction::Unknown {
            return encode_error(
                id,
                "invalid_params",
                "unknown placement action (promote, restore or close)",
            );
        }
        use crate::browser::placements::ActionRefused;
        match crate::browser::placements::board().queue_action(&params.id, params.action) {
            Ok(_) => encode_success(id, ResponseResult::Ok {}),
            Err(ActionRefused::NotFound) => encode_error(
                id,
                "not_found",
                format!("no client holds placement {}", params.id.trim()),
            ),
            Err(ActionRefused::Unsupported) => encode_error(
                id,
                "unsupported",
                "this herdr client predates the bar's expand and close; use its own ⤢ / ✕ or restart the client",
            ),
        }
    }

    /// `browser.host_state`: every herdr tab (or `tab_ids`) with its group.
    pub(super) fn handle_browser_host_state(
        &mut self,
        id: String,
        params: BrowserHostStateParams,
    ) -> String {
        if !hub().lease_matches(&params.lease_id) {
            return error(id, crate::browser::hub::lease_unknown());
        }
        let focused_tab_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_tab_id(ws_idx, ws.active_tab_index())
        });
        let mut tabs = Vec::new();
        for ws_idx in 0..self.state.workspaces.len() {
            let workspace_id = self.public_workspace_id(ws_idx);
            for tab_idx in 0..self.state.workspaces[ws_idx].tabs.len() {
                let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
                    continue;
                };
                if params
                    .tab_ids
                    .as_ref()
                    .is_some_and(|wanted| !wanted.contains(&tab_id))
                {
                    continue;
                }
                let ws = &self.state.workspaces[ws_idx];
                let label = ws.tab_display_name(tab_idx).unwrap_or_default();
                let browser = ws.tabs[tab_idx].browser.as_ref();
                tabs.push(BrowserHostTabState {
                    tab_id,
                    workspace_id: workspace_id.clone(),
                    label,
                    key: browser.map(|browser| browser.key.clone()),
                    rev: browser.map(|browser| browser.rev).unwrap_or(0),
                    active: browser.and_then(|browser| browser.active.clone()),
                    entries: browser.map(TabBrowser::wire_entries).unwrap_or_default(),
                });
            }
        }
        encode_success(
            id,
            ResponseResult::BrowserHostState {
                state: BrowserHostStateInfo {
                    focused_tab_id,
                    tabs,
                },
            },
        )
    }

    /// `browser.host_sync`: the companion's live lists, one full replace
    /// per group sent. Order, url, kind, file and active changes schedule
    /// a save; a title-only change waits for the next one.
    pub(super) fn handle_browser_host_sync(
        &mut self,
        id: String,
        params: BrowserHostSyncParams,
    ) -> String {
        let size = serde_json::to_vec(&params.groups).map_or(0, |bytes| bytes.len());
        if size > MAX_SYNC_BYTES {
            return error(
                id,
                BrowserError::new(
                    "too_large",
                    format!("a sync of {size} bytes; at most {MAX_SYNC_BYTES} (send only the changed groups)"),
                ),
            );
        }
        if let Err(err) = hub().accept_host_sync_seq(&params.lease_id, params.seq) {
            return error(id, err);
        }
        let mut keys = BTreeMap::new();
        let mut unknown_tabs = Vec::new();
        let mut save = false;
        let mut changed = false;
        let mut rewatch = Vec::new();
        let mut bound = Vec::new();
        for group in &params.groups {
            let Some((ws_idx, tab_idx)) = self.parse_tab_id(&group.tab_id) else {
                unknown_tabs.push(group.tab_id.clone());
                continue;
            };
            let tab = &mut self.state.workspaces[ws_idx].tabs[tab_idx];
            let browser = tab
                .browser
                .get_or_insert_with(|| TabBrowser::new(group.key.as_deref()));
            if group.key.as_deref() != Some(browser.key.as_str()) {
                keys.insert(group.tab_id.clone(), browser.key.clone());
            }
            let before = browser.clone();
            let change = browser.apply_sync(&group.tab_id, group, &mut keys);
            save |= change.persisted;
            changed |= change.any;
            rewatch.extend(browser.recreated_html_views(&before));
            bound.extend(browser.new_targets(&before));
        }
        if save {
            self.schedule_session_save();
        }
        if changed {
            hub().note_session_change();
        }
        hub().bind_companion_entries(&bound);
        hub().rewatch_html_views(rewatch);
        encode_success(id, ResponseResult::BrowserHostSynced { keys, unknown_tabs })
    }

    /// `browser.host_report`: the user's strip actions into the ledger as
    /// the user; `shown` clears the group's needs-you; page errors go to
    /// the agent like the sidecar's. Over the rate: dropped, never refused.
    pub(super) fn handle_browser_host_report(
        &mut self,
        id: String,
        params: BrowserHostReportParams,
    ) -> String {
        // `shown` (the user looks: needs-you clears) is never rate limited, so a page
        // that errors in a loop cannot starve it; everything else shares the bucket.
        let is_shown = |event: &crate::api::schema::BrowserHostReportEvent| {
            event.kind == BrowserHostReportKind::UserAction
                && event.action.as_deref().map(str::trim) == Some("shown")
        };
        let limited = params
            .events
            .iter()
            .filter(|event| !is_shown(event))
            .count();
        let mut budget = match hub().admit_host_report(&params.lease_id, limited) {
            Ok(admitted) => admitted,
            Err(err) => return error(id, err),
        };
        let mut shown_left = crate::browser::hub::REPORT_MAX_EVENTS;
        let mut accepted = 0u32;
        for event in &params.events {
            let slot = if is_shown(event) {
                &mut shown_left
            } else {
                &mut budget
            };
            if *slot == 0 {
                continue;
            }
            *slot -= 1;
            match event.kind {
                BrowserHostReportKind::UserAction => {
                    let Some(action) = event
                        .action
                        .as_deref()
                        .map(str::trim)
                        .filter(|action| !action.is_empty())
                    else {
                        continue;
                    };
                    let action: String = action
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .take(32)
                        .collect();
                    hub().record_user_action(
                        event.target.as_deref(),
                        &action,
                        event.url.as_deref(),
                    );
                    if action == "shown" {
                        let targets = event
                            .tab_id
                            .as_deref()
                            .and_then(|tab_id| self.parse_tab_id(tab_id))
                            .and_then(|(ws_idx, tab_idx)| {
                                self.state.workspaces[ws_idx].tabs[tab_idx].browser.as_ref()
                            })
                            .map(|browser| {
                                browser
                                    .entries
                                    .iter()
                                    .filter_map(|entry| entry.target.clone())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_else(|| event.target.clone().into_iter().collect());
                        hub().clear_attention(COMPANION_PROFILE, &targets);
                    }
                    accepted += 1;
                }
                BrowserHostReportKind::PageError => {
                    if let Some(target) = event.target.as_deref() {
                        hub().note_host_page_error(
                            target,
                            event.error.as_deref().unwrap_or("page_error"),
                            event.text.as_deref().unwrap_or(""),
                        );
                        accepted += 1;
                    }
                }
                BrowserHostReportKind::Unknown => {}
            }
        }
        // (the supervisor persists the ledger; no file I/O on this thread)
        let dropped = params.events.len() as u32 - accepted;
        encode_success(
            id,
            ResponseResult::BrowserHostReported { accepted, dropped },
        )
    }

    /// The agent pane a page from herdr tab `(ws_idx, tab_idx)` goes to:
    /// the focused pane when it hosts an agent, else the agent pane that
    /// used the active page most recently, else the first agent pane.
    pub(crate) fn send_page_target(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::layout::PaneId> {
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        let is_agent = |pane: crate::layout::PaneId| {
            self.pane_info(ws_idx, pane)
                .is_some_and(|info| info.agent.is_some())
        };
        let focused = tab.layout.focused();
        if is_agent(focused) {
            return Some(focused);
        }
        let panes = tab.layout.pane_ids();
        let last_user = tab
            .browser
            .as_ref()
            .and_then(TabBrowser::active_entry)
            .and_then(|entry| entry.target.as_deref())
            .and_then(|target| hub().last_user_of(target))
            .and_then(|pane| self.parse_pane_id(&pane))
            .filter(|(user_ws, pane)| *user_ws == ws_idx && panes.contains(pane))
            .map(|(_, pane)| pane)
            .filter(|pane| is_agent(*pane));
        last_user.or_else(|| panes.into_iter().find(|pane| is_agent(*pane)))
    }

    /// `browser.send_page`: the page, the user's selection and note, as a
    /// message from "you (browser)" to an agent of the tab. Needs the live
    /// companion lease (`lease_unknown` otherwise).
    pub(super) fn handle_browser_send_page(
        &mut self,
        id: String,
        params: BrowserSendPageParams,
    ) -> String {
        // The message says it is from the user ("you (browser)"): only the companion that
        // holds the live lease may send it.
        if !params
            .lease_id
            .as_deref()
            .is_some_and(|lease| hub().lease_matches(lease))
        {
            return error(id, crate::browser::hub::lease_unknown());
        }
        let body = params.url.len()
            + params.title.len()
            + params.selection.as_ref().map_or(0, String::len)
            + params.note.as_ref().map_or(0, String::len);
        if body > MAX_SEND_PAGE_BYTES {
            return error(
                id,
                BrowserError::new(
                    "too_large",
                    format!("{body} bytes; at most {MAX_SEND_PAGE_BYTES}"),
                ),
            );
        }
        if params.url.trim().is_empty() {
            return error(
                id,
                BrowserError::new("invalid_request", "send_page needs a url"),
            );
        }
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return error(
                id,
                BrowserError::new("tab_not_found", format!("no herdr tab {}", params.tab_id)),
            );
        };
        let Some(pane) = self.send_page_target(ws_idx, tab_idx) else {
            return error(
                id,
                BrowserError::new(
                    "no_agent_in_tab",
                    format!("no agent runs in herdr tab {}", params.tab_id),
                ),
            );
        };
        let to_pane = self.public_pane_id(ws_idx, pane).unwrap_or_default();
        let Ok(target) = self.resolve_agent_target(&to_pane) else {
            return error(
                id,
                BrowserError::new("no_agent_in_tab", format!("{to_pane} is not running")),
            );
        };
        let to_name = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == target.terminal_id)
            .and_then(|terminal| terminal.agent_name.clone())
            .or_else(|| self.pane_info(ws_idx, pane).and_then(|info| info.agent))
            .unwrap_or_else(|| to_pane.clone());
        let message_id = crate::coordinator::messages::new_id();
        let text = send_page_text(&params);
        let now = crate::coordinator::now_unix();
        let message = AgentMessage {
            unix: now,
            from_pane: None,
            from_name: Some(SEND_PAGE_FROM.into()),
            to_pane: to_pane.clone(),
            to_name: Some(to_name.clone()),
            text: text.clone(),
            outcome: OUTCOME_QUEUED.into(),
            id: Some(message_id.clone()),
            ..AgentMessage::default()
        };
        let answer = |outcome: &str, reason: Option<String>| {
            encode_success(
                id.clone(),
                ResponseResult::BrowserSendPage {
                    outcome: outcome.into(),
                    to_pane: to_pane.clone(),
                    to_name: to_name.clone(),
                    message_id: message_id.clone(),
                    reason,
                },
            )
        };
        // Do-not-disturb: stored quietly (released when it ends), never typed in.
        if self.pane_in_dnd(target.ws_idx, target.pane_id) {
            let release = super::quiet_messages::QuietRelease {
                envelope: text.clone(),
                pointer: None,
                from_terminal: None,
                from_coordinator: false,
            };
            return match self.store_quiet_message(&target, message, true, Some(release), now) {
                Ok(()) => answer("queued", Some(super::quiet_messages::DND_REASON.into())),
                Err((code, text)) => encode_error(id, &code, text),
            };
        }
        let check = self.agent_message_check(&target);
        let pointer = crate::agents_model::envelope::PointerFrom {
            id: message_id.clone(),
            name: "browser".into(),
            ..Default::default()
        };
        match self.deliver_agent_message(
            &format!("browser-send-page:{message_id}"),
            &target,
            check,
            message.clone(),
            text,
            pointer,
            None,
            false,
            now,
        ) {
            Ok(super::message_queue::MessageDelivery::Sent) => {
                self.log_send_page(AgentMessage {
                    outcome: OUTCOME_SENT.into(),
                    ..message
                });
                answer("sent", None)
            }
            Ok(super::message_queue::MessageDelivery::Queued { reason }) => {
                answer("queued", Some(reason))
            }
            Err(refused) => encode_error(id.clone(), &refused.code, refused.message),
        }
    }

    /// When the loop should look at queued page errors next (a batch
    /// window is short; nothing queued, no deadline).
    pub(crate) fn next_browser_page_error_deadline(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        hub()
            .page_errors_pending()
            .then(|| now + crate::browser::page_errors::BATCH_WINDOW / 3)
    }

    /// Store each due page-error batch quietly for its agent when that
    /// agent takes quiet messages (Claude); others pull (`browser console`).
    /// Never typed in, never a wake-up, never urgent. It supersedes the
    /// agent's notice for the same tab still waiting
    /// ([`PageErrorBatch::coalesce_key`](crate::browser::page_errors::PageErrorBatch::coalesce_key)).
    pub(crate) fn handle_browser_page_errors(&mut self) -> bool {
        let batches = hub().take_page_errors();
        let now = crate::coordinator::now_unix();
        let mut stored = false;
        for batch in batches {
            let Ok(target) = self.resolve_agent_target(&batch.pane) else {
                continue;
            };
            if !self.takes_quiet_messages(&target) {
                continue;
            }
            let dnd = self.pane_in_dnd(target.ws_idx, target.pane_id);
            let message = AgentMessage {
                unix: now,
                from_pane: None,
                from_name: Some("herdr browser".into()),
                to_pane: batch.pane.clone(),
                text: batch.message_text(),
                outcome: crate::coordinator::messages::OUTCOME_QUIET.into(),
                id: Some(crate::coordinator::messages::new_id()),
                ..AgentMessage::default()
            };
            let key = batch.coalesce_key();
            match self.store_coalesced_quiet_message(&target, message, dnd, key, now) {
                Ok(()) => stored = true,
                Err((code, _)) => {
                    tracing::debug!(event = "browser.page_errors", code = %code, pane = %batch.pane, "page errors not stored")
                }
            }
        }
        stored
    }

    /// The message log line of a page typed in now (queued ones the queue logs).
    fn log_send_page(&self, message: AgentMessage) {
        let Some(dir) = self.agents_model.dir.clone() else {
            return;
        };
        if let Err(err) = crate::coordinator::messages::append(&dir, &message) {
            tracing::warn!(err = %err, "browser send_page: cannot append to the message log");
        }
    }

    /// `browser.get`'s `sessions`: per herdr tab with pages, its active
    /// page and whether a page needs the user.
    pub(crate) fn browser_sessions(&self) -> Vec<BrowserSessionInfo> {
        let live = hub().companion_live();
        let needing = hub().targets_needing_user(COMPANION_PROFILE);
        let mut sessions = Vec::new();
        for ws_idx in 0..self.state.workspaces.len() {
            for (tab_idx, tab) in self.state.workspaces[ws_idx].tabs.iter().enumerate() {
                let Some(browser) = tab.browser.as_ref().filter(|b| !b.entries.is_empty()) else {
                    continue;
                };
                let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
                    continue;
                };
                let active = browser.active_entry();
                sessions.push(BrowserSessionInfo {
                    tab_id,
                    title: active.map(|entry| entry.title.clone()).unwrap_or_default(),
                    url: active.map(|entry| entry.url.clone()).unwrap_or_default(),
                    pages: browser.entries.len() as u32,
                    needs_you: browser.entries.iter().any(|entry| {
                        entry
                            .target
                            .as_ref()
                            .is_some_and(|target| needing.contains(target))
                    }),
                    host: if live { "companion" } else { "none" }.into(),
                });
            }
        }
        sessions
    }
}

#[cfg(test)]
mod tests;
