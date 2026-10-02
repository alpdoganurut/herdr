//! Agent cards (fork): the sticky notices agents show their user through
//! `agent.notify` (`agents_notify` / `herdr agent notify`).
//!
//! Each endpoint pushes its whole card list as an `endpoint.agent-notices.v1`
//! control message (`EndpointControlMessage::AgentNotices`); the client keeps
//! one `AgentCardsState` per endpoint and replaces it when the server boot
//! differs or the revision is higher. Cards are independent of the toast
//! machinery: no `ui.toast.herdr.sticky`, no pane replacement — a Finished
//! toast for the same pane leaves the card alone.
//!
//! A card stays until the user dismisses it (`×`, a right click, the fold
//! line for all) or visits the agent's tab (the server clears it and pushes
//! the new list). A click on a card focuses the agent's pane — on another
//! machine through `ActivateEndpoint`.
//!
//! Sound: ids not seen before for that endpoint's boot ring once per payload
//! (question → request, done → done; info and warning are silent) and go to
//! the terminal or the system per `ui.toast.delivery`; never a herdr toast.
//! The first payload after attach or reconnect (`initial`) only seeds the
//! seen set.
//!
//! The look is fixed (the Dusk palette, independent of the theme), drawn by
//! the pure `render_agent_cards`; an empty card set adds no work.

use std::collections::{HashMap, HashSet};

use super::render::put_text;
use super::*;
use crate::api::schema::{AgentNoticeDismissParams, AgentNoticeInfo, AgentNoticeKind, Method};
use crate::server::headless::agent_notices::AgentNoticesPayload;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::style::Color;

/// Dusk: card background, border, name, tab, age / ×, body.
pub(super) const CARD_BG: Color = Color::Rgb(0x1d, 0x1f, 0x27);
pub(super) const CARD_BORDER: Color = Color::Rgb(0x2c, 0x2f, 0x3b);
pub(super) const CARD_TEXT: Color = Color::Rgb(0xcf, 0xd2, 0xde);
pub(super) const CARD_TAB: Color = Color::Rgb(0x8b, 0x8f, 0xa3);
pub(super) const CARD_DIM: Color = Color::Rgb(0x5d, 0x61, 0x74);
pub(super) const CARD_BODY: Color = Color::Rgb(0xa6, 0xaa, 0xbb);

/// The widest card.
pub(super) const CARD_WIDTH: u16 = 52;
/// Cards drawn before the rest fold into one line.
pub(super) const MAX_DRAWN: usize = 4;
/// Body lines a card shows.
const BODY_LINES: usize = 2;

/// A kind's glyph and accent.
pub(super) fn kind_accent(kind: AgentNoticeKind) -> (&'static str, Color) {
    match kind {
        AgentNoticeKind::Question => ("?", Color::Rgb(0xe8, 0xb8, 0x6b)),
        AgentNoticeKind::Done => ("✓", Color::Rgb(0x5f, 0xd4, 0xc4)),
        AgentNoticeKind::Warning => ("!", Color::Rgb(0xff, 0x6b, 0x7f)),
        AgentNoticeKind::Info | AgentNoticeKind::Unknown => ("●", Color::Rgb(0x6e, 0xa8, 0xfe)),
    }
}

/// One endpoint's cards as its last accepted payload listed them.
#[derive(Debug, Default)]
pub(super) struct AgentCardsState {
    pub(super) boot_id: String,
    pub(super) revision: u64,
    /// Every card, oldest first.
    pub(super) notices: Vec<AgentNoticeInfo>,
    /// Ids already shown for this boot (they never ring again).
    pub(super) seen: HashSet<String>,
    /// A payload was accepted for this boot.
    pub(super) seeded: bool,
    /// Ids dismissed here and not yet gone from the server's list (a
    /// non-active machine, or a server without `agent.notice_dismiss`).
    pub(super) hidden: HashSet<String>,
}

impl AgentCardsState {
    fn visible(&self) -> impl Iterator<Item = &AgentNoticeInfo> {
        self.notices
            .iter()
            .filter(|notice| !self.hidden.contains(&notice.id))
    }
}

/// What a press on the card stack hits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentCardHit {
    /// The card: focus the agent's pane.
    Card {
        endpoint_id: ClientEndpointId,
        id: String,
        pane_id: String,
        tab_id: Option<String>,
    },
    /// The card's `×`: dismiss it.
    Close {
        endpoint_id: ClientEndpointId,
        id: String,
    },
    /// `+N more from agents`: dismiss every card.
    Fold,
}

/// A card to draw: its machine and the notice.
pub(super) struct AgentCardView<'a> {
    pub(super) endpoint_id: &'a ClientEndpointId,
    pub(super) notice: &'a AgentNoticeInfo,
}

/// `now`, `2m`, `3h`, `1d`.
pub(super) fn age_label(now_unix: u64, unix: u64) -> String {
    let age = now_unix.saturating_sub(unix);
    match age {
        0..=59 => "now".into(),
        60..=3_599 => format!("{}m", age / 60),
        3_600..=86_399 => format!("{}h", age / 3_600),
        _ => format!("{}d", age / 86_400),
    }
}

fn width_of(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// The longest prefix of `text` that fits `width` cells.
fn take_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// `text` cut to `width` cells, with `…` when cut.
pub(super) fn fit(text: &str, width: usize) -> String {
    if width_of(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    format!("{}…", take_width(text, width - 1))
}

/// The body wrapped to `width`, at most `BODY_LINES` lines, `…` on the last
/// when more is left.
pub(super) fn body_lines(body: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut wrapped = Vec::new();
    for source in body.lines() {
        let mut line = String::new();
        let mut used = 0;
        for ch in source.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + w > width && !line.is_empty() {
                wrapped.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(ch);
            used += w;
        }
        wrapped.push(line);
        if wrapped.len() > BODY_LINES {
            break;
        }
    }
    while wrapped.last().is_some_and(|line| line.trim().is_empty()) {
        wrapped.pop();
    }
    if wrapped.len() > BODY_LINES {
        wrapped.truncate(BODY_LINES);
        if let Some(last) = wrapped.last_mut() {
            *last = format!("{}…", take_width(last, width - 1));
        }
    }
    wrapped
}

fn card_height(notice: &AgentNoticeInfo, inner_width: usize) -> u16 {
    let body = notice
        .body
        .as_deref()
        .map(|body| body_lines(body, inner_width.saturating_sub(2)).len())
        .unwrap_or(0);
    4 + body as u16
}

fn fill(buffer: &mut Buffer, rect: Rect, style: Style) {
    let rect = rect.intersection(buffer.area);
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            buffer[(x, y)].reset();
            buffer[(x, y)].set_symbol(" ").set_style(style);
        }
    }
}

/// One card at `rect`. Returns its `×` cell.
fn render_card(buffer: &mut Buffer, rect: Rect, notice: &AgentNoticeInfo, now_unix: u64) -> Rect {
    let bg = Style::default().bg(CARD_BG);
    fill(buffer, rect, bg);
    let border = bg.fg(CARD_BORDER);
    let right = rect.right().saturating_sub(1);
    let bottom = rect.bottom().saturating_sub(1);
    let inner_width = rect.width.saturating_sub(2);
    put_text(buffer, rect.x, rect.y, 1, "╭", border);
    put_text(buffer, right, rect.y, 1, "╮", border);
    put_text(buffer, rect.x, bottom, 1, "╰", border);
    put_text(buffer, right, bottom, 1, "╯", border);
    let rule = "─".repeat(usize::from(inner_width));
    put_text(buffer, rect.x + 1, rect.y, inner_width, &rule, border);
    put_text(buffer, rect.x + 1, bottom, inner_width, &rule, border);
    for y in rect.y + 1..bottom {
        put_text(buffer, rect.x, y, 1, "│", border);
        put_text(buffer, right, y, 1, "│", border);
    }
    let (glyph, accent) = kind_accent(notice.kind);
    let accent_style = bg.fg(accent);
    let x0 = rect.x + 1;
    for y in rect.y + 1..bottom {
        put_text(buffer, x0, y, 1, "▌", accent_style);
    }
    // header: glyph, name, · tab, then age and × at the right
    let header_y = rect.y + 1;
    let age = age_label(now_unix, notice.unix);
    let tail = format!("{age}  × ");
    let tail_width = width_of(&tail) as u16;
    let tail_x = right.saturating_sub(tail_width);
    let text_x = x0 + 2;
    let head_room = usize::from(tail_x.saturating_sub(text_x).saturating_sub(1));
    put_text(
        buffer,
        text_x,
        header_y,
        1,
        glyph,
        accent_style.add_modifier(Modifier::BOLD),
    );
    let name_x = text_x + 2;
    let name_room = head_room.saturating_sub(2);
    let name = fit(&notice.name, name_room);
    put_text(
        buffer,
        name_x,
        header_y,
        name_room as u16,
        &name,
        bg.fg(CARD_TEXT).add_modifier(Modifier::BOLD),
    );
    if let Some(tab) = notice.tab_label.as_deref().filter(|tab| !tab.is_empty()) {
        let used = width_of(&name);
        let room = name_room.saturating_sub(used);
        if room > 4 {
            let tab = fit(&format!(" · {tab}"), room);
            put_text(
                buffer,
                name_x + used as u16,
                header_y,
                room as u16,
                &tab,
                bg.fg(CARD_TAB),
            );
        }
    }
    put_text(buffer, tail_x, header_y, tail_width, &tail, bg.fg(CARD_DIM));
    let close = Rect::new(right.saturating_sub(3), header_y, 3, 1);
    // title
    let room = usize::from(right.saturating_sub(text_x).saturating_sub(1));
    put_text(
        buffer,
        text_x,
        header_y + 1,
        room as u16,
        &fit(&notice.title, room),
        bg.fg(CARD_TEXT).add_modifier(Modifier::BOLD),
    );
    if let Some(body) = notice.body.as_deref() {
        for (offset, line) in body_lines(body, room).iter().enumerate() {
            let y = header_y + 2 + offset as u16;
            if y >= bottom {
                break;
            }
            put_text(buffer, text_x, y, room as u16, line, bg.fg(CARD_BODY));
        }
    }
    close
}

/// The card stack, newest first, at the top right of `area` starting
/// `top_offset` rows down: at most `MAX_DRAWN` cards (fewer when the area
/// is short); the rest fold into `+N more from agents`. Pure: draws and
/// returns the hits (each card's `×` before the card itself).
pub(super) fn render_agent_cards(
    buffer: &mut Buffer,
    area: Rect,
    top_offset: u16,
    cards: &[AgentCardView<'_>],
    now_unix: u64,
) -> Vec<(Rect, AgentCardHit)> {
    let mut hits = Vec::new();
    let width = CARD_WIDTH.min(area.width);
    if cards.is_empty() || width < 16 || area.height == 0 {
        return hits;
    }
    let x = area.right().saturating_sub(width);
    let mut y = area.y.saturating_add(top_offset);
    let mut drawn = 0;
    for (index, card) in cards.iter().enumerate() {
        let height = card_height(card.notice, usize::from(width.saturating_sub(2)));
        let fold_reserve = u16::from(index + 1 < cards.len());
        if drawn >= MAX_DRAWN
            || y.saturating_add(height).saturating_add(fold_reserve) > area.bottom()
        {
            break;
        }
        let rect = Rect::new(x, y, width, height);
        let close = render_card(buffer, rect, card.notice, now_unix);
        hits.push((
            close,
            AgentCardHit::Close {
                endpoint_id: card.endpoint_id.clone(),
                id: card.notice.id.clone(),
            },
        ));
        hits.push((
            rect,
            AgentCardHit::Card {
                endpoint_id: card.endpoint_id.clone(),
                id: card.notice.id.clone(),
                pane_id: card.notice.pane_id.clone(),
                tab_id: card.notice.tab_id.clone(),
            },
        ));
        y = y.saturating_add(height);
        drawn += 1;
    }
    let folded = cards.len() - drawn;
    if folded > 0 && y < area.bottom() {
        let text = format!(" +{folded} more from agents ");
        let fold_width = (width_of(&text) as u16).min(area.width);
        let rect = Rect::new(area.right().saturating_sub(fold_width), y, fold_width, 1);
        fill(buffer, rect, Style::default().bg(CARD_BG));
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &text,
            Style::default().fg(CARD_TAB).bg(CARD_BG),
        );
        hits.push((rect, AgentCardHit::Fold));
    }
    hits
}

/// The mobile layout: one banner line for the newest card (its glyph in its
/// accent), `+N more` in the body when there are others.
pub(super) fn render_agent_card_banner(
    buffer: &mut Buffer,
    area: Rect,
    cards: &[AgentCardView<'_>],
    offset_for_warning: bool,
    palette: &Palette,
) -> Vec<(Rect, AgentCardHit)> {
    let Some(newest) = cards.first() else {
        return Vec::new();
    };
    let notice = newest.notice;
    let (glyph, accent) = kind_accent(notice.kind);
    let title = format!("{}: {}", notice.name, notice.title);
    let body = if cards.len() > 1 {
        Some(format!("+{} more", cards.len() - 1))
    } else {
        notice.tab_label.clone()
    };
    let rect = super::notifications::render_mobile_notice_banner(
        buffer,
        area,
        &title,
        body.as_deref(),
        accent,
        offset_for_warning,
        palette,
    );
    let dot = (rect.x.saturating_add(1), rect.y);
    if !rect.is_empty() && buffer.area.contains(dot.into()) {
        buffer[dot].set_symbol(glyph);
    }
    if rect.is_empty() {
        return Vec::new();
    }
    vec![(
        rect,
        AgentCardHit::Card {
            endpoint_id: newest.endpoint_id.clone(),
            id: notice.id.clone(),
            pane_id: notice.pane_id.clone(),
            tab_id: notice.tab_id.clone(),
        },
    )]
}

impl ClientShellState {
    /// An `endpoint.agent-notices.v1` payload from `endpoint_id`. Returns the
    /// effects (sound, terminal / system notices for new cards) and whether
    /// the frame must be recomposed.
    pub(crate) fn receive_agent_notices(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: AgentNoticesPayload,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        let cards = self.agent_cards.entry(endpoint_id.clone()).or_default();
        let same_boot = cards.seeded && cards.boot_id == payload.boot_id;
        if same_boot && !payload.initial && payload.revision <= cards.revision {
            return (Vec::new(), false);
        }
        if !same_boot {
            cards.seen.clear();
            cards.hidden.clear();
        }
        let first = payload.initial || !same_boot;
        let fresh: Vec<AgentNoticeInfo> = if first {
            Vec::new()
        } else {
            payload
                .notices
                .iter()
                .filter(|notice| !cards.seen.contains(&notice.id))
                .cloned()
                .collect()
        };
        cards.seen = payload
            .notices
            .iter()
            .map(|notice| notice.id.clone())
            .collect();
        let present = &cards.seen;
        cards.hidden.retain(|id| present.contains(id));
        cards.boot_id = payload.boot_id;
        cards.revision = payload.revision;
        cards.notices = payload.notices;
        cards.seeded = true;
        let effects = self.agent_card_effects(endpoint_id, &fresh);
        (effects, true)
    }

    fn agent_card_effects(
        &self,
        endpoint_id: &ClientEndpointId,
        fresh: &[AgentNoticeInfo],
    ) -> Vec<ClientShellNotificationEffect> {
        let mut effects = Vec::new();
        if fresh.is_empty() {
            return effects;
        }
        let ring = fresh
            .iter()
            .find(|notice| notice.kind == AgentNoticeKind::Question)
            .map(|notice| (crate::sound::Sound::Request, notice))
            .or_else(|| {
                fresh
                    .iter()
                    .find(|notice| notice.kind == AgentNoticeKind::Done)
                    .map(|notice| (crate::sound::Sound::Done, notice))
            });
        if let Some((sound, notice)) = ring {
            effects.push(ClientShellNotificationEffect::Sound {
                sound,
                agent: notice.agent.clone(),
            });
        }
        for notice in fresh {
            let active = self.agent_card_target_active(endpoint_id, notice);
            if active && self.outer_focused != Some(false) {
                continue;
            }
            let title = format!("{}: {}", notice.name, notice.title);
            let body = notice.body.clone();
            match self.config.toast_delivery {
                crate::config::ToastDelivery::Terminal => {
                    effects.push(ClientShellNotificationEffect::Terminal { title, body });
                }
                crate::config::ToastDelivery::System => {
                    effects.push(ClientShellNotificationEffect::System { title, body });
                }
                // the card is the in-herdr notice; no toast on top
                crate::config::ToastDelivery::Herdr | crate::config::ToastDelivery::Off => {}
            }
        }
        effects
    }

    fn agent_card_target_active(
        &self,
        endpoint_id: &ClientEndpointId,
        notice: &AgentNoticeInfo,
    ) -> bool {
        endpoint_id == &self.active_endpoint_id
            && notice.tab_id.is_some()
            && self
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.focused_tab_id == notice.tab_id)
    }

    /// The boot an endpoint's snapshot reports: `None` for a machine no
    /// longer in the list, `Some(None)` before its first snapshot.
    fn agent_cards_endpoint_boot(&self, endpoint_id: &ClientEndpointId) -> Option<Option<&str>> {
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)?;
        let snapshot = if endpoint_id == &self.active_endpoint_id {
            self.snapshot.as_deref().or(endpoint.snapshot.as_deref())
        } else {
            endpoint.snapshot.as_deref()
        };
        Some(snapshot.map(|snapshot| snapshot.boot_id.as_str()))
    }

    /// Whether any card would be drawn: no allocation, a few map entries.
    pub(super) fn has_agent_cards(&self) -> bool {
        !self.agent_cards.is_empty()
            && self
                .agent_cards
                .iter()
                .any(|(endpoint_id, cards)| self.agent_cards_shown(endpoint_id, cards))
    }

    fn agent_cards_shown(&self, endpoint_id: &ClientEndpointId, cards: &AgentCardsState) -> bool {
        // A server that restarted with no card sends no list: its old cards
        // go when the endpoint's snapshot reports another boot. A machine
        // removed from the list takes its cards along.
        cards.visible().next().is_some()
            && self
                .agent_cards_endpoint_boot(endpoint_id)
                .is_some_and(|boot| boot.is_none_or(|boot| boot == cards.boot_id))
    }

    /// Every card to draw, newest first (only called when there are some).
    pub(super) fn agent_card_views(&self) -> Vec<AgentCardView<'_>> {
        let mut views: Vec<(usize, AgentCardView<'_>)> = Vec::new();
        for (endpoint_id, cards) in &self.agent_cards {
            if !self.agent_cards_shown(endpoint_id, cards) {
                continue;
            }
            for (index, notice) in cards.notices.iter().enumerate() {
                if !cards.hidden.contains(&notice.id) {
                    views.push((
                        index,
                        AgentCardView {
                            endpoint_id,
                            notice,
                        },
                    ));
                }
            }
        }
        // newest first; within one endpoint, later in its list is newer
        views.sort_by(|(ia, a), (ib, b)| {
            b.notice
                .unix
                .cmp(&a.notice.unix)
                .then(ib.cmp(ia))
                .then_with(|| a.notice.id.cmp(&b.notice.id))
        });
        views.into_iter().map(|(_, view)| view).collect()
    }

    /// A press on the card stack. Every card (and the fold line) swallows
    /// its presses: left focuses the agent, `×` or right dismisses, the fold
    /// line dismisses every card.
    pub(super) fn handle_agent_card_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if self.hits.agent_cards.is_empty() {
            return false;
        }
        let point = (mouse.column, mouse.row);
        let Some(hit) = self
            .hits
            .agent_cards
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, hit)| hit.clone())
        else {
            return false;
        };
        match (hit, mouse.kind) {
            (AgentCardHit::Fold, MouseEventKind::Down(MouseButton::Left | MouseButton::Right)) => {
                self.dismiss_all_agent_cards(outcome);
            }
            (
                AgentCardHit::Close { endpoint_id, id }
                | AgentCardHit::Card {
                    endpoint_id, id, ..
                },
                MouseEventKind::Down(MouseButton::Right),
            )
            | (AgentCardHit::Close { endpoint_id, id }, MouseEventKind::Down(MouseButton::Left)) => {
                self.dismiss_agent_card(&endpoint_id, id, outcome);
            }
            (
                AgentCardHit::Card {
                    endpoint_id,
                    id,
                    pane_id,
                    tab_id,
                },
                MouseEventKind::Down(MouseButton::Left),
            ) => {
                self.focus_agent_card(&endpoint_id, id, pane_id, tab_id, outcome);
            }
            (_, MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_)) => {}
            _ => return false,
        }
        true
    }

    fn agent_notice_dismiss_supported(&self, endpoint_id: &ClientEndpointId) -> bool {
        endpoint_id == &self.active_endpoint_id
            && self.endpoint_is_online(endpoint_id)
            && self.supports_endpoint_method(&Method::AgentNoticeDismiss(
                AgentNoticeDismissParams::default(),
            ))
    }

    /// `×`: hide the card here and, on the active machine, tell the server
    /// (which removes it on every client). Elsewhere it hides locally only.
    pub(super) fn dismiss_agent_card(
        &mut self,
        endpoint_id: &ClientEndpointId,
        id: String,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(cards) = self.agent_cards.get_mut(endpoint_id) {
            cards.hidden.insert(id.clone());
        }
        outcome.repaint = true;
        if self.agent_notice_dismiss_supported(endpoint_id) {
            self.push_endpoint_method(
                Method::AgentNoticeDismiss(AgentNoticeDismissParams {
                    ids: vec![id],
                    all: false,
                }),
                outcome,
            );
        }
    }

    /// The fold line: every card goes (the active machine's through the
    /// server, `all: true`).
    pub(super) fn dismiss_all_agent_cards(&mut self, outcome: &mut ClientShellInput) {
        let active = self.active_endpoint_id.clone();
        let mut active_had_cards = false;
        for (endpoint_id, cards) in &mut self.agent_cards {
            let ids: Vec<String> = cards.notices.iter().map(|n| n.id.clone()).collect();
            if endpoint_id == &active && !ids.is_empty() {
                active_had_cards = true;
            }
            cards.hidden.extend(ids);
        }
        outcome.repaint = true;
        if active_had_cards && self.agent_notice_dismiss_supported(&active) {
            self.push_endpoint_method(
                Method::AgentNoticeDismiss(AgentNoticeDismissParams {
                    ids: Vec::new(),
                    all: true,
                }),
                outcome,
            );
        }
    }

    /// A click on a card: focus the agent's pane (client-driven, so the
    /// server clears the card when the tab changes). Already on that tab,
    /// the visit cannot clear it: the click dismisses it too.
    fn focus_agent_card(
        &mut self,
        endpoint_id: &ClientEndpointId,
        id: String,
        pane_id: String,
        tab_id: Option<String>,
        outcome: &mut ClientShellInput,
    ) {
        if !self.endpoint_is_online(endpoint_id) {
            let label = self.endpoint_label(endpoint_id).to_owned();
            self.receive_endpoint_unavailable(format!("{label} is unavailable"));
            outcome.repaint = true;
            return;
        }
        if endpoint_id == &self.active_endpoint_id {
            let on_tab = tab_id.is_some()
                && self
                    .snapshot
                    .as_deref()
                    .is_some_and(|snapshot| snapshot.focused_tab_id == tab_id);
            self.push_endpoint_method(
                Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
                outcome,
            );
            if on_tab {
                self.dismiss_agent_card(endpoint_id, id, outcome);
            }
        } else {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id: endpoint_id.clone(),
                target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
            });
        }
        outcome.repaint = true;
    }
}

/// A patch row lands on a drawn card: the card would be overwritten until
/// the next full compose.
pub(super) fn agent_cards_block_patch(
    state: &ClientShellState,
    patch: &crate::protocol::PaneSurfacePatch,
) -> bool {
    if state.hits.agent_cards.is_empty() {
        return false;
    }
    let (cols, rows) = state.last_composed_size.unwrap_or_default();
    let area = state.layout(cols, rows).pane_surface;
    patch.rows.iter().any(|row| {
        let rect = Rect::new(
            area.x.saturating_add(row.x),
            area.y.saturating_add(row.y),
            u16::try_from(row.cells.len()).unwrap_or(u16::MAX),
            1,
        );
        state
            .hits
            .agent_cards
            .iter()
            .any(|(card, _)| card.intersects(rect))
    })
}

/// The state field's type: one card list per endpoint.
pub(super) type AgentCardsByEndpoint = HashMap<ClientEndpointId, AgentCardsState>;
