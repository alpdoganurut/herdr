//! Agent notices (fork): the sticky cards agents show their user
//! (`agent.notify`, `agent.notices`, `agent.notice_dismiss`).
//!
//! [`AgentNotices`] is pure server-owned data: one card per sender (a new
//! notice replaces the sender's card), at most [`MAX_NOTICES`], rate-limited
//! per sender, text sanitized. The sender is the pane's own record (agent
//! name, else agent kind, else pane id), never a parameter. Cards are not
//! persisted and do not survive a live handoff.
//!
//! A card goes away when the user dismisses it, when its pane is gone, or
//! when the user's client moves into the card's tab (client-driven focus
//! only, see `focus_shell_client_on_tab`). Every mutation bumps
//! [`AgentNotices::revision`]; the render pass pushes the list to every
//! client shell whose last sent revision differs.

use std::collections::{HashMap, VecDeque};

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::agent_notices::error_code;
use crate::api::schema::{
    AgentNoticeDismissParams, AgentNoticeInfo, AgentNoticeKind, AgentNotifyOutcome,
    AgentNotifyParams, ResponseResult,
};

/// Cards kept at once; the oldest is dropped past it.
pub(crate) const MAX_NOTICES: usize = 16;
pub(crate) const TITLE_MAX_CHARS: usize = 80;
pub(crate) const BODY_MAX_CHARS: usize = 280;
pub(crate) const BODY_MAX_LINES: usize = 3;
/// A duplicate (same title and body as the sender's current card) inside
/// this window refreshes the card instead of using a rate-limit slot.
const DEDUPE_WINDOW_S: u64 = 600;
/// Per sender: at most `max` shown notices in any `window_s` seconds.
const RATE_LIMITS: [(u64, usize); 3] = [(20, 1), (600, 5), (3600, 20)];
const LEDGER_HORIZON_S: u64 = 3600;

/// One card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentNotice {
    /// `n<seq>`.
    pub(crate) id: String,
    /// The sender's agent session id, else its pane id when the card was made.
    pub(crate) sender_key: String,
    /// The sender's public pane id now (remapped when the pane moves).
    pub(crate) pane_id: String,
    pub(crate) agent: Option<String>,
    pub(crate) name: String,
    pub(crate) kind: AgentNoticeKind,
    pub(crate) title: String,
    pub(crate) body: Option<String>,
    pub(crate) unix: u64,
}

/// Who sends a notice, from the server's own pane record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoticeSender {
    pub(crate) key: String,
    pub(crate) pane_id: String,
    pub(crate) agent: Option<String>,
    pub(crate) name: String,
}

/// Why `agent.notify` refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotifyRefusal {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl NotifyRefusal {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AgentNotices {
    list: Vec<AgentNotice>,
    revision: u64,
    seq: u64,
    /// Shown-notice times per sender key, newest last, the last hour only.
    ledger: HashMap<String, VecDeque<u64>>,
    enabled: bool,
}

impl Default for AgentNotices {
    fn default() -> Self {
        Self::new(true)
    }
}

impl AgentNotices {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            list: Vec::new(),
            revision: 0,
            seq: 0,
            ledger: HashMap::new(),
            enabled,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Bumped by every mutation; `0` until the first card ever.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Oldest first.
    pub(crate) fn list(&self) -> &[AgentNotice] {
        &self.list
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// `[agents] notices`; turning it off removes every card.
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled && !self.list.is_empty() {
            self.list.clear();
            self.bump();
        }
    }

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1).max(1);
    }

    /// The cards' projection changed (a tab or space label): clients are
    /// sent the list again.
    pub(crate) fn touch(&mut self) {
        self.bump();
    }

    /// Show a card. `title` and `body` must already be sanitized
    /// ([`sanitize_title`], [`sanitize_body`]).
    pub(crate) fn notify(
        &mut self,
        sender: NoticeSender,
        kind: AgentNoticeKind,
        title: String,
        body: Option<String>,
        now: u64,
    ) -> Result<(String, AgentNotifyOutcome), NotifyRefusal> {
        if !self.enabled {
            return Err(NotifyRefusal::new(
                error_code::NOTICES_OFF,
                "agent notices are off ([agents] notices = false)",
            ));
        }
        if let Some(card) = self.list.iter_mut().find(|card| {
            card.sender_key == sender.key
                && card.title == title
                && card.body == body
                && now.saturating_sub(card.unix) < DEDUPE_WINDOW_S
        }) {
            // The refreshed age shows with the next push; a duplicate alone
            // pushes nothing (an agent repeating itself costs no render).
            card.unix = now;
            let id = card.id.clone();
            return Ok((id, AgentNotifyOutcome::Deduped));
        }
        self.prune_ledger(now);
        if let Some(stamps) = self.ledger.get(&sender.key) {
            let retry = RATE_LIMITS
                .iter()
                .filter_map(|&(window, max)| {
                    let inside: Vec<u64> = stamps
                        .iter()
                        .copied()
                        .filter(|&t| now.saturating_sub(t) < window)
                        .collect();
                    (inside.len() >= max).then(|| {
                        let oldest = inside.iter().copied().min().unwrap_or(now);
                        (oldest + window).saturating_sub(now).max(1)
                    })
                })
                .max();
            if let Some(retry) = retry {
                return Err(NotifyRefusal::new(
                    error_code::RATE_LIMITED,
                    format!("too many notices from this agent; retry in {retry}s"),
                ));
            }
        }
        self.ledger
            .entry(sender.key.clone())
            .or_default()
            .push_back(now);
        self.seq += 1;
        let id = format!("n{}", self.seq);
        self.list.retain(|card| card.sender_key != sender.key);
        self.list.push(AgentNotice {
            id: id.clone(),
            sender_key: sender.key,
            pane_id: sender.pane_id,
            agent: sender.agent,
            name: sender.name,
            kind,
            title,
            body,
            unix: now,
        });
        if self.list.len() > MAX_NOTICES {
            let excess = self.list.len() - MAX_NOTICES;
            self.list.drain(..excess);
        }
        self.bump();
        Ok((id, AgentNotifyOutcome::Shown))
    }

    fn prune_ledger(&mut self, now: u64) {
        self.ledger.retain(|_, stamps| {
            while stamps
                .front()
                .is_some_and(|&t| now.saturating_sub(t) >= LEDGER_HORIZON_S)
            {
                stamps.pop_front();
            }
            !stamps.is_empty()
        });
    }

    /// Remove the cards `ids` (or every card); whether any went.
    pub(crate) fn dismiss(&mut self, ids: &[String], all: bool) -> bool {
        let before = self.list.len();
        if all {
            self.list.clear();
        } else {
            self.list.retain(|card| !ids.contains(&card.id));
        }
        let changed = self.list.len() != before;
        if changed {
            self.bump();
        }
        changed
    }

    /// Keep the cards `keep` accepts; whether any went.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&AgentNotice) -> bool) -> bool {
        let before = self.list.len();
        self.list.retain(|card| keep(card));
        let changed = self.list.len() != before;
        if changed {
            self.bump();
        }
        changed
    }

    /// Point cards at their pane's current id: `remap(old)` is the new id,
    /// `None` when the pane is gone (the card goes). Whether anything changed.
    pub(crate) fn follow_panes(&mut self, mut remap: impl FnMut(&str) -> Option<String>) -> bool {
        let mut changed = false;
        self.list.retain_mut(|card| match remap(&card.pane_id) {
            Some(pane_id) => {
                if pane_id != card.pane_id {
                    card.pane_id = pane_id;
                    changed = true;
                }
                true
            }
            None => {
                changed = true;
                false
            }
        });
        if changed {
            self.bump();
        }
        changed
    }
}

// ----- sanitizing -------------------------------------------------------------

fn is_invisible(ch: char) -> bool {
    matches!(ch,
        '\u{7f}'..='\u{9f}'
        | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}'
        | '\u{feff}')
}

/// Drop escape sequences, C0/C1 controls (keeping `\n` when `keep_newlines`;
/// tabs and line separators become spaces or newlines), DEL, bidi
/// overrides and zero-width characters.
fn strip_controls(text: &str, keep_newlines: bool) -> String {
    let newline = if keep_newlines { '\n' } else { ' ' };
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.next() {
                // CSI: parameters up to a final byte.
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: a string up to BEL or ESC \.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // A two-character escape (or a trailing ESC).
                _ => {}
            },
            '\n' | '\u{2028}' | '\u{2029}' => out.push(newline),
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.push(newline);
                }
            }
            '\t' => out.push(' '),
            ch if ch < ' ' || is_invisible(ch) => {}
            ch => out.push(ch),
        }
    }
    out
}

fn collapse_spaces(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` cut to `max` characters, ending in `…` when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// One line, at most [`TITLE_MAX_CHARS`]; `None` when nothing is left.
pub(crate) fn sanitize_title(raw: &str) -> Option<String> {
    let title = collapse_spaces(&strip_controls(raw, false));
    (!title.is_empty()).then(|| cut(&title, TITLE_MAX_CHARS))
}

/// At most [`BODY_MAX_LINES`] non-empty lines and [`BODY_MAX_CHARS`]
/// characters; `None` when nothing is left.
pub(crate) fn sanitize_body(raw: &str) -> Option<String> {
    let stripped = strip_controls(raw, true);
    let lines: Vec<String> = stripped
        .split('\n')
        .map(collapse_spaces)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let cut_lines = lines.len() > BODY_MAX_LINES;
    let mut body = lines[..lines.len().min(BODY_MAX_LINES)].join("\n");
    if body.chars().count() > BODY_MAX_CHARS {
        return Some(cut(&body, BODY_MAX_CHARS));
    }
    if cut_lines {
        if body.chars().count() >= BODY_MAX_CHARS {
            body = cut(&body, BODY_MAX_CHARS);
        } else {
            body.push('…');
        }
    }
    Some(body)
}

// ----- the App side -----------------------------------------------------------

impl App {
    /// `agent.notify`.
    pub(super) fn handle_agent_notify(&mut self, id: String, params: AgentNotifyParams) -> String {
        let Some(title) = sanitize_title(&params.title) else {
            return encode_error(
                id,
                error_code::INVALID_PARAMS,
                "agent.notify: the title is empty",
            );
        };
        let body = params.body.as_deref().and_then(sanitize_body);
        let Some(sender) = self.notice_sender(&params.caller_pane) else {
            return encode_error(
                id,
                error_code::PANE_NOT_FOUND,
                format!("pane {} not found", params.caller_pane),
            );
        };
        let kind = match params.kind {
            AgentNoticeKind::Unknown => AgentNoticeKind::Info,
            kind => kind,
        };
        let pane_id = sender.pane_id.clone();
        match self
            .agent_notices
            .notify(sender, kind, title, body, crate::coordinator::now_unix())
        {
            Ok((notice_id, outcome)) => {
                // Only a new card renders (refusals and duplicates never do).
                if outcome == AgentNotifyOutcome::Shown {
                    self.render_dirty.request_generic();
                    self.render_notify.notify_one();
                }
                tracing::info!(
                    event = "agent.notify",
                    pane = %pane_id,
                    id = %notice_id,
                    kind = kind.as_str(),
                    ?outcome,
                    "agent notice"
                );
                encode_success(
                    id,
                    ResponseResult::AgentNotify {
                        id: notice_id,
                        outcome,
                    },
                )
            }
            Err(refusal) => {
                tracing::debug!(
                    event = "agent.notify",
                    pane = %pane_id,
                    code = refusal.code,
                    "agent notice refused"
                );
                encode_error(id, refusal.code, refusal.message)
            }
        }
    }

    /// `agent.notices`.
    pub(super) fn handle_agent_notices(&mut self, id: String) -> String {
        self.follow_agent_notice_panes();
        encode_success(
            id,
            ResponseResult::AgentNotices {
                notices: self.agent_notice_infos(),
            },
        )
    }

    /// `agent.notice_dismiss`.
    pub(super) fn handle_agent_notice_dismiss(
        &mut self,
        id: String,
        params: AgentNoticeDismissParams,
    ) -> String {
        self.agent_notices.dismiss(&params.ids, params.all);
        encode_success(
            id,
            ResponseResult::AgentNotices {
                notices: self.agent_notice_infos(),
            },
        )
    }

    /// The sender behind `caller_pane`, from the server's pane record.
    fn notice_sender(&self, caller_pane: &str) -> Option<NoticeSender> {
        let (ws_idx, raw) = self.resolve_caller_pane(caller_pane)?;
        let pane_id = self.public_pane_id(ws_idx, raw)?;
        let pane = self.pane_info(ws_idx, raw)?;
        let agent_name = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(raw))
            .and_then(|state| self.state.terminals.get(&state.attached_terminal_id))
            .and_then(|terminal| terminal.agent_name.clone())
            .filter(|name| !name.trim().is_empty());
        let agent = pane.agent.filter(|agent| !agent.trim().is_empty());
        let key = pane
            .agent_session
            .map(|session| session.value)
            .filter(|value| !value.is_empty())
            .map(|value| format!("session:{value}"))
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        let name = agent_name
            .or_else(|| agent.clone())
            .unwrap_or_else(|| pane_id.clone());
        Some(NoticeSender {
            key,
            pane_id,
            agent,
            name,
        })
    }

    /// Cards with their tab and workspace resolved now; a card whose pane
    /// no longer resolves is left out.
    pub(crate) fn agent_notice_infos(&self) -> Vec<AgentNoticeInfo> {
        self.agent_notices
            .list()
            .iter()
            .filter_map(|card| {
                let (ws_idx, raw) = self.parse_pane_id(&card.pane_id)?;
                let ws = self.state.workspaces.get(ws_idx)?;
                let tab_idx = ws.find_tab_index_for_pane(raw);
                let workspace_label =
                    ws.display_name_from(&self.state.terminals, &self.terminal_runtimes);
                Some(AgentNoticeInfo {
                    id: card.id.clone(),
                    kind: card.kind,
                    title: card.title.clone(),
                    body: card.body.clone(),
                    agent: card.agent.clone(),
                    name: card.name.clone(),
                    pane_id: self
                        .public_pane_id(ws_idx, raw)
                        .unwrap_or_else(|| card.pane_id.clone()),
                    tab_id: tab_idx.and_then(|tab_idx| self.public_tab_id(ws_idx, tab_idx)),
                    tab_label: tab_idx.and_then(|tab_idx| ws.tab_display_name(tab_idx)),
                    workspace_id: Some(self.public_workspace_id(ws_idx)),
                    workspace_label: Some(workspace_label).filter(|label| !label.is_empty()),
                    unix: card.unix,
                })
            })
            .collect()
    }

    /// After a structural change (a pane moved, closed or exited, a tab or
    /// space closed): cards follow their pane's new id; cards of gone panes
    /// go. O(cards) and nothing at all without cards.
    pub(crate) fn follow_agent_notice_panes(&mut self) {
        if self.agent_notices.is_empty() {
            return;
        }
        let current: Vec<Option<String>> = self
            .agent_notices
            .list()
            .iter()
            .map(|card| {
                self.parse_pane_id(&card.pane_id)
                    .and_then(|(ws_idx, raw)| self.public_pane_id(ws_idx, raw))
            })
            .collect();
        let mut current = current.into_iter();
        if self
            .agent_notices
            .follow_panes(|_| current.next().flatten())
        {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
    }

    /// A tab or space was renamed: cards in it show the new label, so the
    /// list is re-sent when one is there. O(cards), nothing without cards.
    pub(crate) fn follow_agent_notice_labels(&mut self, data: &crate::api::schema::EventData) {
        use crate::api::schema::EventData;
        if self.agent_notices.is_empty() {
            return;
        }
        let renamed = match data {
            EventData::TabRenamed { tab_id, .. } => self
                .parse_tab_id(tab_id)
                .map(|(ws_idx, tab_idx)| (ws_idx, Some(tab_idx))),
            EventData::WorkspaceRenamed { workspace_id, .. } => self
                .parse_workspace_id(workspace_id)
                .filter(|ws_idx| self.state.workspaces.get(*ws_idx).is_some())
                .map(|ws_idx| (ws_idx, None)),
            _ => None,
        };
        let Some((ws_idx, tab_idx)) = renamed else {
            return;
        };
        let affected = self.agent_notices.list().iter().any(|card| {
            self.parse_pane_id(&card.pane_id)
                .is_some_and(|(card_ws, raw)| {
                    card_ws == ws_idx
                        && tab_idx.is_none_or(|tab_idx| {
                            self.state.workspaces[card_ws].find_tab_index_for_pane(raw)
                                == Some(tab_idx)
                        })
                })
        });
        if affected {
            self.agent_notices.touch();
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
    }

    /// A client moved into `tab_id` by the user's own action: the cards of
    /// agents in that tab are seen. Whether any went.
    pub(crate) fn visit_agent_notices_tab(&mut self, tab_id: &str) -> bool {
        if self.agent_notices.is_empty() {
            return false;
        }
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(tab_id) else {
            return false;
        };
        let in_tab: Vec<bool> = self
            .agent_notices
            .list()
            .iter()
            .map(|card| {
                self.parse_pane_id(&card.pane_id)
                    .is_some_and(|(card_ws, raw)| {
                        card_ws == ws_idx
                            && self.state.workspaces[card_ws].find_tab_index_for_pane(raw)
                                == Some(tab_idx)
                    })
            })
            .collect();
        let mut in_tab = in_tab.into_iter();
        let changed = self
            .agent_notices
            .retain(|_| !in_tab.next().unwrap_or(false));
        if changed {
            tracing::debug!(event = "agent.notices.visited", tab = tab_id, "cards seen");
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;

    fn sender(key: &str, pane: &str) -> NoticeSender {
        NoticeSender {
            key: key.into(),
            pane_id: pane.into(),
            agent: Some("claude".into()),
            name: key.into(),
        }
    }

    fn show(
        notices: &mut AgentNotices,
        key: &str,
        title: &str,
        now: u64,
    ) -> Result<(String, AgentNotifyOutcome), NotifyRefusal> {
        notices.notify(
            sender(key, "w1:p1"),
            AgentNoticeKind::Question,
            title.into(),
            None,
            now,
        )
    }

    #[test]
    fn titles_are_one_clean_line_and_never_empty() {
        assert_eq!(
            sanitize_title("  hello \n  world\t!  ").unwrap(),
            "hello world !"
        );
        assert_eq!(
            sanitize_title("\u{1b}[31mred\u{1b}[0m \u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{1b}\\")
                .unwrap(),
            "red link"
        );
        assert_eq!(
            sanitize_title("a\u{202e}b\u{2066}c\u{200b}d\u{feff}e\u{7f}f\u{85}g\u{7}h").unwrap(),
            "abcdefgh"
        );
        assert_eq!(sanitize_title(" \u{200b}\u{1b}[2J \n"), None);
        let long = sanitize_title(&"x".repeat(200)).unwrap();
        assert_eq!(long.chars().count(), TITLE_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn bodies_keep_three_lines_and_280_characters() {
        assert_eq!(
            sanitize_body("one\r\n\n  two  words \n\u{1b}[1mthree\u{1b}[0m").unwrap(),
            "one\ntwo words\nthree"
        );
        assert_eq!(sanitize_body("1\n2\n3\n4\n5").unwrap(), "1\n2\n3…");
        let long = sanitize_body(&"y".repeat(400)).unwrap();
        assert_eq!(long.chars().count(), BODY_MAX_CHARS);
        assert!(long.ends_with('…'));
        assert_eq!(sanitize_body("\n \u{202a}\n"), None);
    }

    #[test]
    fn a_sender_gets_one_card_and_a_new_notice_replaces_it() {
        let mut notices = AgentNotices::default();
        let (first, outcome) = show(&mut notices, "a", "first", T0).unwrap();
        assert_eq!(outcome, AgentNotifyOutcome::Shown);
        show(&mut notices, "b", "other", T0).unwrap();
        let (second, _) = show(&mut notices, "a", "second", T0 + 30).unwrap();
        assert_ne!(first, second);
        let titles: Vec<&str> = notices.list().iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["other", "second"]);
    }

    #[test]
    fn duplicates_refresh_the_card_without_using_a_slot() {
        let mut notices = AgentNotices::default();
        let (id, _) = show(&mut notices, "a", "same", T0).unwrap();
        let revision = notices.revision();
        let (again, outcome) = show(&mut notices, "a", "same", T0 + 5).unwrap();
        assert_eq!(
            (again.as_str(), outcome),
            (id.as_str(), AgentNotifyOutcome::Deduped)
        );
        assert_eq!(notices.list()[0].unix, T0 + 5);
        assert_eq!(notices.revision(), revision, "a duplicate pushes nothing");
        // A different title inside 20 s is still limited: the duplicate used no slot.
        let refused = show(&mut notices, "a", "new", T0 + 6).unwrap_err();
        assert_eq!(refused.code, error_code::RATE_LIMITED);
        assert!(
            refused.message.contains("retry in 14s"),
            "{}",
            refused.message
        );
        // Past 10 minutes the same text is a new card.
        let (later, outcome) = show(&mut notices, "a", "same", T0 + 5 + 600).unwrap();
        assert_eq!(outcome, AgentNotifyOutcome::Shown);
        assert_ne!(later, id);
    }

    #[test]
    fn rate_limits_hold_per_sender_over_every_window() {
        let mut notices = AgentNotices::default();
        // 1 per 20 s.
        show(&mut notices, "a", "t0", T0).unwrap();
        assert_eq!(
            show(&mut notices, "a", "t1", T0 + 19).unwrap_err().code,
            error_code::RATE_LIMITED
        );
        // Another sender is not limited by it.
        show(&mut notices, "b", "t0", T0 + 1).unwrap();
        // 5 per 10 minutes.
        for i in 1..5 {
            show(&mut notices, "a", &format!("t{i}"), T0 + 20 * i).unwrap();
        }
        let refused = show(&mut notices, "a", "t5", T0 + 100).unwrap_err();
        assert!(
            refused.message.contains("retry in 500s"),
            "{}",
            refused.message
        );
        show(&mut notices, "a", "t5", T0 + 600).unwrap();
        // 20 per hour.
        let mut now = T0 + 600;
        let mut shown = 6;
        while shown < 20 {
            now += 125;
            show(&mut notices, "a", &format!("u{shown}"), now).unwrap();
            shown += 1;
        }
        assert!(now < T0 + 3600);
        let refused = show(&mut notices, "a", "over", now + 125).unwrap_err();
        assert_eq!(refused.code, error_code::RATE_LIMITED);
        show(&mut notices, "a", "fresh", T0 + 3600).unwrap();
    }

    #[test]
    fn the_list_is_capped_oldest_first() {
        let mut notices = AgentNotices::default();
        for i in 0..(MAX_NOTICES + 3) {
            show(&mut notices, &format!("s{i}"), &format!("t{i}"), T0).unwrap();
        }
        assert_eq!(notices.list().len(), MAX_NOTICES);
        assert_eq!(notices.list()[0].title, "t3");
    }

    #[test]
    fn notices_off_refuses_and_clears() {
        let mut notices = AgentNotices::default();
        show(&mut notices, "a", "t", T0).unwrap();
        notices.set_enabled(false);
        assert!(notices.is_empty());
        assert_eq!(
            show(&mut notices, "b", "t", T0).unwrap_err().code,
            error_code::NOTICES_OFF
        );
    }

    #[test]
    fn dismiss_follow_and_retain_bump_the_revision_only_on_change() {
        let mut notices = AgentNotices::default();
        assert_eq!(notices.revision(), 0);
        let (a, _) = show(&mut notices, "a", "t", T0).unwrap();
        show(&mut notices, "b", "t", T0).unwrap();
        let revision = notices.revision();
        assert!(!notices.dismiss(&["nope".into()], false));
        assert_eq!(notices.revision(), revision);
        assert!(notices.dismiss(std::slice::from_ref(&a), false));
        assert!(notices.revision() > revision);
        let revision = notices.revision();
        assert!(!notices.follow_panes(|pane| Some(pane.to_string())));
        assert_eq!(notices.revision(), revision);
        assert!(notices.follow_panes(|_| Some("w2:p9".into())));
        assert_eq!(notices.list()[0].pane_id, "w2:p9");
        assert!(notices.follow_panes(|_| None));
        assert!(notices.is_empty());
        show(&mut notices, "c", "t", T0 + 100).unwrap();
        assert!(notices.dismiss(&[], true));
        assert!(notices.is_empty());
    }
}
