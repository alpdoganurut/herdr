//! The session header (fork, header design: the Codex mock,
//! `.local/prd/header-design/codex.html`): the `tabs` layout's one row on
//! top of the session area, in place of the sidebar's detail strip and the
//! view strip.
//!
//! Left: the focused tab's status as an icon only (its state colour), the
//! tab's group when it is in one (`overlay1`, then ` · `; named as the
//! sidebar's group header names it: the team's purpose, else the group
//! label), the tab's name (bold), then its context use from 75 % (the tab row's rule),
//! `⚭N` running subagents and `✉N` quiet messages waiting, each only when
//! there is something to say. Right: the view tabs `Terminal` / `Browser`
//! and the browser dock toggle `◨` (while the companion browser docks
//! here; `Browser` carries the tab's open page count in a small block,
//! `surface1` or accent while Browser view is on, none at 0 pages or while
//! the companion is stopped, `99+` from 100), then, at the far right, the notes dock toggle `≡`. The active
//! view and the open dock are accent; everything else is dim and a hovered
//! control turns `text`: text colour only, no bars or boxes. Narrow: the
//! name truncates first (to `MIN_NAME` cells), then the view tabs shrink to
//! `T  B ◨` (`B7 ◨` with pages), then to `◨` (no count); the dock toggles always stay. Once only `◨` is
//! left the facts stay whole and the name keeps shrinking below `MIN_NAME`
//! (`● head…  82% ⚭3 ✉1  ◨  ≡` at 26 columns); the facts drop only when
//! not even one name cell fits beside them. The group never takes room from
//! the rest: it gets only the cells the whole name leaves, shortens (`…`)
//! and drops once fewer than `MIN_GROUP` of its cells fit.
//!
//! Client presentation only. Built once per compose from snapshot borrows
//! and static strings: one pass over the snapshot's agents for the focused
//! tab (its subagents, context use and resume marks) and one over its
//! spaces for the group, never per pane, and
//! no allocation (the counts are `StackStr`s, the hits fixed rects).

use std::fmt::Write as _;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use super::render::{display_width, put_text, put_truncated};
use super::sidebar_model::StackStr;
use super::*;
use crate::api::schema::AgentStatus;

/// The browser dock toggle (and the browser dock's title mark).
pub(crate) const BROWSER_DOCK_GLYPH: &str = "\u{25E8}"; // ◨
/// The notes dock toggle (and the notes dock's title mark).
pub(crate) const NOTES_DOCK_GLYPH: &str = "\u{2261}"; // ≡
/// A tab whose agent restarts or wakes.
const RESUMING_GLYPH: &str = "\u{21BB}"; // ↻
/// A tab that waits on the user (the dots style; the sidebar's `●`).
const BLOCKED_GLYPH: &str = "\u{25C9}"; // ◉
/// The cells the name keeps before the view tabs shrink.
pub(crate) const MIN_NAME: u16 = 8;
/// The fewest group cells drawn (a shorter group shows whole or not at all).
const MIN_GROUP: u16 = 4;
/// Between the group and the tab name (the toasts' `group · role`).
const GROUP_SEPARATOR: &str = " \u{00B7} "; //  ·
/// The narrowest notice line on the header (Browser view).
const BANNER_MIN: u16 = 16;

/// Where the desktop toasts, endpoint notices, the lifecycle banner and the
/// config diagnostic stack: the frame below the header row while the header
/// is drawn (so they never cover it), else the whole frame (upstream). Only
/// the top moves; bottom-anchored toasts keep the frame's bottom.
pub(crate) fn toast_area(layout: &ClientShellLayout, cols: u16, rows: u16) -> Rect {
    let top = if layout.session_header.is_empty() {
        0
    } else {
        layout.session_header.bottom().min(rows)
    };
    Rect::new(0, top, cols, rows - top)
}

/// Where the config diagnostic and the endpoint status banner go while the
/// header is drawn (they right-align and get no dock rect of their own):
/// in Browser view the header's banner slot (`browser_banner`; the
/// companion covers the rest of the tab, and the slot keeps the view tabs
/// and dock toggles clickable), else `toast_area` left of the info dock or
/// the browser band with one blank column, like a right-anchored toast (the
/// companion's window covers the band). Without the header: `toast_area`
/// (upstream).
pub(crate) fn status_banner_area(
    layout: &ClientShellLayout,
    toast_area: Rect,
    browser_banner: Option<Rect>,
) -> Rect {
    if layout.session_header.is_empty() {
        return toast_area;
    }
    if let Some(slot) = browser_banner {
        return slot;
    }
    let dock = super::browser_dock::toast_dock(layout);
    if dock.is_empty() || dock.x <= toast_area.x.saturating_add(1) {
        return toast_area;
    }
    Rect::new(
        toast_area.x,
        toast_area.y,
        dock.x - 1 - toast_area.x,
        toast_area.height,
    )
}

/// The header's banner slot left of `left` (where a status banner starts),
/// one blank column before it, for the notice or toast; the whole slot (the
/// notice drawn over the banners) when fewer than `BANNER_MIN` cells are
/// left.
pub(crate) fn slot_left_of(slot: Rect, left: u16) -> Rect {
    let width = left.saturating_sub(1).saturating_sub(slot.x);
    if width >= BANNER_MIN {
        Rect::new(slot.x, slot.y, width, slot.height)
    } else {
        slot
    }
}

/// A click target of the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderTarget {
    ViewTerminal,
    ViewBrowser,
    BrowserDock,
    NotesDock,
}

/// What the header drew (fixed rects: nothing allocated per frame).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionHeaderHits {
    pub(crate) area: Rect,
    pub(crate) terminal: Rect,
    pub(crate) browser: Rect,
    pub(crate) browser_dock: Rect,
    pub(crate) notes_dock: Rect,
    /// Where a notice shows while the tab is in Browser view: the gap
    /// between the facts and the view tabs (or after the icon when that is
    /// too narrow); empty when no line of `BANNER_MIN` cells fits.
    pub(crate) banner: Rect,
}

impl SessionHeaderHits {
    pub(crate) fn resolve(&self, point: (u16, u16)) -> Option<HeaderTarget> {
        [
            (self.terminal, HeaderTarget::ViewTerminal),
            (self.browser, HeaderTarget::ViewBrowser),
            (self.browser_dock, HeaderTarget::BrowserDock),
            (self.notes_dock, HeaderTarget::NotesDock),
        ]
        .into_iter()
        .find(|(rect, _)| super::contains(*rect, point))
        .map(|(_, target)| target)
    }
}

/// The focused tab's browser controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeaderBrowser {
    /// Browser view is on.
    pub(crate) viewing: bool,
    /// The browser dock is open (lit only while Terminal shows).
    pub(crate) docked: bool,
    /// The tab's open companion pages (0 while the companion is stopped):
    /// the badge after `Browser`, none at 0.
    pub(crate) pages: u32,
}

/// The page count badge's text: `7`, `12`, `99+` from 100 (empty at 0).
fn page_count(pages: u32) -> StackStr<4> {
    let mut text = StackStr::<4>::new();
    match pages {
        0 => {}
        1..=99 => {
            let _ = write!(text, "{pages}");
        }
        _ => {
            let _ = text.write_str("99+");
        }
    }
    text
}

/// One frame of the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionHeader<'a> {
    pub(crate) icon: &'static str,
    pub(crate) icon_color: Color,
    /// The tab's group (`teams::group_name`), `None` outside a group.
    pub(crate) group: Option<&'a str>,
    pub(crate) name: &'a str,
    pub(crate) name_color: Color,
    /// `NN%` from 75 % and its colour.
    pub(crate) context: Option<(&'static str, Color)>,
    pub(crate) subagents: u32,
    /// `✉N` and its colour.
    pub(crate) mail: Option<(&'static str, Color)>,
    /// `None`: the companion browser does not dock here (no view tabs, no `◨`).
    pub(crate) browser: Option<HeaderBrowser>,
    pub(crate) notes_docked: bool,
    pub(crate) hover: Option<HeaderTarget>,
}

/// The status icon: `●` working (and done), `○` idle, `◉` blocked, `◌`
/// suspended, `↻` restarting or waking; `ui.status_indicators = "symbols"`
/// keeps the sidebar's symbol set.
pub(crate) fn status_glyph(
    status: AgentStatus,
    resuming: bool,
    style: crate::config::StatusIndicatorStyle,
) -> &'static str {
    if resuming {
        return RESUMING_GLYPH;
    }
    match (style, status) {
        (crate::config::StatusIndicatorStyle::Dots, AgentStatus::Blocked) => BLOCKED_GLYPH,
        _ => super::status_icon(status, style),
    }
}

/// The status icon's colour: the sidebar's state colours, yellow while
/// restarting or waking.
pub(crate) fn status_glyph_color(status: AgentStatus, resuming: bool, palette: &Palette) -> Color {
    if resuming {
        palette.yellow
    } else {
        super::status_color(status, palette)
    }
}

/// The view tabs' forms, widest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewForm {
    Full,
    Short,
    Toggle,
}

impl ViewForm {
    fn labels(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Full => Some(("Terminal", "Browser")),
            Self::Short => Some(("T", "B")),
            Self::Toggle => None,
        }
    }

    /// The badge's cells for a count of `digits` cells (none at 0): ` 7 `
    /// after a blank in the full form, `7` right after `B` in the short one,
    /// no count once only `◨` is left.
    fn badge_width(self, digits: u16) -> u16 {
        match self {
            _ if digits == 0 => 0,
            Self::Full => 1 + digits + 2,
            Self::Short => digits,
            Self::Toggle => 0,
        }
    }

    /// `Terminal  Browser ◨`, `T  B ◨`, `◨`, plus the badge.
    fn width(self, digits: u16) -> u16 {
        match self.labels() {
            Some((terminal, browser)) => {
                display_width(terminal) + 2 + display_width(browser) + self.badge_width(digits) + 2
            }
            None => 1,
        }
    }
}

/// The facts after the name: `NN%`, `⚭N`, `✉N`, one blank apart.
fn facts_width(header: &SessionHeader<'_>, subagents: &str) -> u16 {
    let mut width = 0u16;
    let mut add = |text: &str| {
        if !text.is_empty() {
            width += if width == 0 { 0 } else { 1 } + display_width(text);
        }
    };
    add(header.context.map_or("", |(text, _)| text));
    add(subagents);
    add(header.mail.map_or("", |(text, _)| text));
    width
}

/// Draw `header` on `area` (one row) and return what was drawn.
pub(crate) fn render(
    buffer: &mut Buffer,
    area: Rect,
    header: &SessionHeader<'_>,
    palette: &Palette,
) -> SessionHeaderHits {
    let mut hits = SessionHeaderHits::default();
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return hits;
    }
    let area = Rect::new(area.x, area.y, area.width, 1);
    hits.area = area;
    let bg = palette.sidebar_chrome();
    buffer.set_style(area, Style::default().bg(bg));
    for x in area.left()..area.right() {
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_symbol(" ");
        }
    }
    let y = area.y;
    let fg = |color: Color| Style::default().fg(color).bg(bg);
    let control = |target: HeaderTarget, lit: bool| {
        if lit {
            fg(palette.accent)
        } else if header.hover == Some(target) {
            fg(palette.text)
        } else {
            fg(palette.overlay0)
        }
    };
    // A one-cell margin at both ends.
    let left = area.x.saturating_add(1);
    let right = area.right().saturating_sub(1);
    if right <= left {
        return hits;
    }
    // The notes toggle, always (the far right).
    let notes_x = right - 1;
    put_text(
        buffer,
        notes_x,
        y,
        1,
        NOTES_DOCK_GLYPH,
        control(HeaderTarget::NotesDock, header.notes_docked),
    );
    hits.notes_dock = Rect::new(notes_x, y, area.right() - notes_x, 1);
    let mut end = notes_x.saturating_sub(2);

    let mut subagents = StackStr::<12>::new();
    if header.subagents > 0 {
        let _ = write!(
            subagents,
            "{}{}",
            super::tab_sidebar::TAB_SUBAGENTS_ICON,
            header.subagents
        );
    }
    let facts = facts_width(header, subagents.as_str());
    let name_x = left.saturating_add(2);
    let name_width = display_width(header.name);
    let wanted_name = name_width.min(MIN_NAME);
    let left_needed = |end: u16| {
        let facts = if facts > 0 { facts + 2 } else { 0 };
        end.saturating_sub(name_x)
            .checked_sub(facts)
            .is_some_and(|room| room >= wanted_name)
    };

    // The view tabs and the dock toggle: the widest form that leaves the
    // name its minimum.
    if let Some(browser) = header.browser {
        let count = page_count(browser.pages);
        let digits = display_width(count.as_str());
        let form = [ViewForm::Full, ViewForm::Short]
            .into_iter()
            .find(|form| {
                end.checked_sub(form.width(digits) + 2)
                    .is_some_and(|rest| rest > name_x && left_needed(rest))
            })
            .unwrap_or(ViewForm::Toggle);
        let start = end.saturating_sub(form.width(digits));
        if start > left {
            let mut x = start;
            if let Some((terminal, browser_label)) = form.labels() {
                let terminal_width = display_width(terminal);
                put_text(
                    buffer,
                    x,
                    y,
                    terminal_width,
                    terminal,
                    control(HeaderTarget::ViewTerminal, !browser.viewing),
                );
                hits.terminal = Rect::new(x, y, terminal_width, 1);
                x += terminal_width + 2;
                let browser_width = display_width(browser_label);
                put_text(
                    buffer,
                    x,
                    y,
                    browser_width,
                    browser_label,
                    control(HeaderTarget::ViewBrowser, browser.viewing),
                );
                // The badge: grey, accent while Browser view is on; part of
                // the Browser button.
                let badge = form.badge_width(digits);
                if badge > 0 {
                    let block = if browser.viewing {
                        Style::default().fg(bg).bg(palette.accent)
                    } else {
                        Style::default().fg(palette.text).bg(palette.surface1)
                    };
                    // Full: a blank, then ` N `; short: `N` right after `B`.
                    let (gap, pad) = if form == ViewForm::Full {
                        (1, 1)
                    } else {
                        (0, 0)
                    };
                    let block_x = x + browser_width + gap;
                    let block_rect = Rect::new(block_x, y, badge - gap, 1).intersection(area);
                    buffer.set_style(block_rect, block);
                    put_text(buffer, block_x + pad, y, digits, count.as_str(), block);
                }
                hits.browser = Rect::new(x, y, browser_width + badge, 1);
                x += browser_width + badge + 1;
            }
            put_text(
                buffer,
                x,
                y,
                1,
                BROWSER_DOCK_GLYPH,
                control(
                    HeaderTarget::BrowserDock,
                    browser.docked && !browser.viewing,
                ),
            );
            hits.browser_dock = Rect::new(x, y, 1, 1);
            end = start.saturating_sub(2);
        }
    }

    // The status icon, the name, the facts.
    put_text(
        buffer,
        left,
        y,
        1.min(end.saturating_sub(left)),
        header.icon,
        fg(header.icon_color),
    );
    let room = end.saturating_sub(name_x);
    let show_facts = facts > 0 && room > facts + 2;
    let name_room = if show_facts { room - facts - 2 } else { room };
    // The group, dim, before the name: only in the cells the whole name
    // leaves, so the name, the facts and the controls keep theirs; in
    // Browser view also the notice line's (`BANNER_MIN` and its gap), so
    // the notice sits where it sits without a group.
    let mut name_at = name_x;
    if let Some(group) = header.group {
        let separator = display_width(GROUP_SEPARATOR);
        let group_width = display_width(group);
        let notice = if header.browser.is_some_and(|browser| browser.viewing) {
            BANNER_MIN + 2
        } else {
            0
        };
        let group_room = name_room
            .saturating_sub(name_width)
            .saturating_sub(separator)
            .saturating_sub(notice)
            .min(group_width);
        if group_width > 0 && group_room >= group_width.min(MIN_GROUP) {
            let group_style = fg(palette.overlay1);
            let group_end = put_truncated(buffer, name_x, y, group_room, group, group_style);
            put_text(
                buffer,
                group_end,
                y,
                separator,
                GROUP_SEPARATOR,
                group_style,
            );
            name_at = group_end + separator;
        }
    }
    let name_room = name_room - (name_at - name_x);
    put_truncated(
        buffer,
        name_at,
        y,
        name_room,
        header.name,
        fg(header.name_color).add_modifier(Modifier::BOLD),
    );
    let mut content_end = name_at + name_width.min(name_room);
    if show_facts {
        let mut x = content_end + 2;
        let mut first = true;
        for (text, color) in [
            header.context.unwrap_or(("", palette.overlay1)),
            (subagents.as_str(), palette.subtext0),
            header.mail.unwrap_or(("", palette.overlay1)),
        ] {
            if text.is_empty() {
                continue;
            }
            if !first {
                x += 1;
            }
            first = false;
            let width = display_width(text);
            put_text(buffer, x, y, width, text, fg(color));
            x += width;
        }
        content_end = x;
    }
    // The notice line in Browser view: after the facts, else over the name.
    for from in [content_end.saturating_add(2), name_x] {
        if end >= from.saturating_add(BANNER_MIN) {
            hits.banner = Rect::new(from, y, end - from, 1);
            break;
        }
    }
    hits
}

impl ClientShellState {
    /// The header of the focused tab (`None` without one). One pass over
    /// the snapshot's agents for that tab.
    pub(crate) fn session_header_view(&self) -> Option<SessionHeader<'_>> {
        let snapshot = self.snapshot.as_deref()?;
        let tab_id = snapshot.focused_tab_id.as_deref()?;
        let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
        let palette = &self.config.palette;
        let context = super::agent_context::active_agent_context_of(
            &self.agent_context,
            &self.active_endpoint_id,
            Some(snapshot),
        )
        .filter(|context| self.config.sidebar_context_usage && !context.panes.is_empty());
        let times = super::agent_times::active_agent_times_of(
            &self.agent_times,
            &self.active_endpoint_id,
            Some(snapshot),
        )
        .filter(|times| times.any_resuming());
        let mut subagents = 0u32;
        let mut usage = None;
        for agent in snapshot
            .agents
            .iter()
            .filter(|agent| agent.tab_id == tab_id)
        {
            if agent.subagents > 0 && super::sidebar_model::shows_subagents(agent.agent_status) {
                subagents = subagents.saturating_add(agent.subagents);
            }
            if let Some(next) = context.and_then(|context| context.usage_of(&agent.pane_id)) {
                usage = Some(super::agent_context::fuller(usage, next));
            }
        }
        // The relaunched agent may be undetected for a while: its pane counts.
        let resuming = times.is_some_and(|times| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.tab_id == tab_id)
                .any(|pane| {
                    times
                        .resume_mark(&pane.pane_id)
                        .is_some_and(super::agent_times::ClientResumeMark::is_resuming)
                })
        });
        let pending =
            super::quiet::active_quiet_of(&self.quiet, &self.active_endpoint_id, Some(snapshot))
                .map_or(0, |quiet| quiet.pending(tab_id));
        let viewing = self.browser_view_open_for(tab_id);
        let docked = self.browser_dock_open_for(tab_id);
        let browser = (self.config.browser_view_strip
            && (self.browser_companion().is_some() || viewing || docked)
            && self.browser_dock_available())
        .then(|| HeaderBrowser {
            viewing,
            docked,
            pages: if self.browser_companion_stopped().is_some() {
                0
            } else {
                self.browser_tab_pages(tab_id)
            },
        });
        let status = tab.agent_status;
        Some(SessionHeader {
            icon: status_glyph(status, resuming, self.config.status_indicators),
            icon_color: status_glyph_color(status, resuming, palette),
            group: super::teams::group_name(snapshot, self.active_teams(), &tab.workspace_id),
            name: &tab.label,
            name_color: super::tab_color::tab_label_fg(tab.color, palette).unwrap_or(palette.text),
            context: usage.and_then(|usage| {
                super::agent_context::row_percent_text(usage).map(|text| {
                    (
                        text,
                        super::agent_context::row_percent_color(usage, palette),
                    )
                })
            }),
            subagents,
            mail: super::quiet::pending_mark(pending, palette),
            browser,
            notes_docked: self.info_dock_open_for_focused_tab()
                && !self.browser_dock_open_for_focused_tab(),
            hover: self.header_hover,
        })
    }

    /// A left press on the header (checked before any pane hit). Returns
    /// whether the press was the header's.
    pub(crate) fn session_header_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let hits = self.hits.session_header;
        if !super::contains(hits.area, point) {
            return false;
        }
        if let Some(target) = hits.resolve(point) {
            self.activate_header_target(target, outcome);
        }
        true
    }

    /// Run a header control for the focused tab.
    pub(crate) fn activate_header_target(
        &mut self,
        target: HeaderTarget,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab_id) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_tab_id.clone())
        else {
            return;
        };
        match target {
            HeaderTarget::ViewTerminal => self.set_browser_view(tab_id, false, outcome),
            HeaderTarget::ViewBrowser => self.set_browser_view(tab_id, true, outcome),
            HeaderTarget::BrowserDock => {
                // In Browser view the toggle brings the terminal back with
                // the browser docked beside it.
                if self.browser_view_open_for(&tab_id) {
                    self.set_browser_view(tab_id.clone(), false, outcome);
                    if !self.browser_dock_open_for(&tab_id) {
                        self.toggle_browser_dock(tab_id, outcome);
                    }
                } else {
                    self.toggle_browser_dock(tab_id, outcome);
                }
            }
            HeaderTarget::NotesDock => self.toggle_info_pane(tab_id, outcome),
        }
    }

    /// The hovered header control (a mouse move): repaints on a change.
    pub(crate) fn update_header_hover(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) {
        let target = self.hits.session_header.resolve(point);
        if target != self.header_hover {
            self.header_hover = target;
            outcome.repaint = true;
        }
    }
}
