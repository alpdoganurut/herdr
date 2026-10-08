//! The tabs sidebar's Active block (fork, sidebar v2): blocked, voice,
//! working, idle-with-subagents and finished agents under the list (sidebar
//! v3; above it before), with their time in state. Hidden entirely while
//! nothing is active or when `ui.sidebar_active_agents = false`.
//!
//! The entries come sorted from `SidebarModel::active` (rebuilt on data
//! change only). A frame draws the hairline rule on the block's first row,
//! the header under it, then at most `rect.height - 2` entry lines (`+N
//! more` / `show fewer` takes the last one when needed), with `put_text` /
//! `put_truncated` and stack strings: no allocation beyond one tab id clone
//! per entry for the hit map.
//!
//! Fork (sidebar v3): the rule-header-entries frame, the header, the entry
//! line and the hover tracking here are shared with the Pinned
//! (`tab_sidebar_pins.rs`) and Scheduled (`tab_sidebar_scheduled.rs`)
//! blocks. An entry in a team group names the team (`◆` and its purpose).
//!
//! Durations come from the `endpoint.agent-times.v1` push
//! (`agent_times.rs`); without it the time column is left out and the order
//! still follows `state_change_seq`. The block records when the first shown
//! duration changes its text (`hits.sidebar_clock_deadline`), so the client
//! repaints at most once a minute and only while a duration shows.

use std::fmt::Write as _;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use super::render::{display_width, put_text, put_truncated, ShellRenderState};
use super::sidebar_model::{
    format_age, next_age_tick, ActiveView, FixedKind, SidebarHover, SidebarModel, StackStr,
    CLASS_BLOCKED, CLASS_DONE, CLASS_SUBAGENTS, CLASS_VOICE, CLASS_WORKING,
};
use super::*;
use crate::api::schema::{AgentStatus, AgentVoiceMode};

/// The block title.
pub(super) const ACTIVE_TITLE: &str = "Active";
/// The overflow row while expanded.
pub(super) const SHOW_FEWER: &str = "show fewer";
/// The most entry lines while expanded.
const EXPANDED_CAP: u16 = 20;
/// The time column's width (`9d23h`).
const TIME_CELLS: u16 = 5;
/// The subagent column's width (`⚭12` and a gap).
const SUBAGENT_CELLS: u16 = 4;
/// The hover bar at x = 0.
pub(super) const HOVER_BAR: &str = "\u{258E}"; // ▎

/// The most entry lines the block shows (folded past it into `+N more`) at
/// a sidebar content height.
pub(super) fn active_cap(content_height: u16) -> u16 {
    if content_height >= 36 {
        8
    } else if content_height >= 28 {
        6
    } else {
        4
    }
}

/// The rows the block takes (its rule, header, entry lines, `+N more`);
/// 0 while disabled or empty.
pub(super) fn active_block_rows(model: &SidebarModel, view: ActiveView, cap: u16) -> u16 {
    section_rows(model.active.len(), view, cap, EXPANDED_CAP)
}

/// The rows a section block takes for `entries` entries at line cap `cap`
/// (rule, header, entry lines, `+N more`); 0 while disabled or empty, 2
/// folded. Shared by Active, Pinned and Scheduled.
pub(super) fn section_rows(entries: usize, view: ActiveView, cap: u16, expanded_cap: u16) -> u16 {
    if !view.enabled || entries == 0 {
        return 0;
    }
    if view.folded {
        return 2;
    }
    let entries = u16::try_from(entries).unwrap_or(u16::MAX);
    let cap = cap.max(1);
    let lines = if entries <= cap {
        entries
    } else if view.expanded {
        // Every entry up to the expanded cap, then `show fewer`.
        entries.min(expanded_cap.max(cap) - 1) + 1
    } else {
        // `cap - 1` entries and `+N more`.
        cap
    };
    lines + 2
}

/// A section block's frame in `rect`: the rule on the first row, the
/// header row under it, and the entry lines after it (0 while folded).
pub(super) fn section_frame(
    buffer: &mut Buffer,
    rect: Rect,
    folded: bool,
    palette: &Palette,
) -> Option<(Rect, u16)> {
    if rect.height < 2 {
        return None;
    }
    put_rule(buffer, rect.x, rect.y, rect.width, palette);
    let header = Rect::new(rect.x, rect.y + 1, rect.width, 1);
    let lines = if folded { 0 } else { rect.height - 2 };
    Some((header, lines))
}

/// A section's overflow row: `+N more` (or `show fewer` for `hidden == 0`)
/// dim at x = 5, with the hover band.
pub(super) fn render_overflow_row(
    buffer: &mut Buffer,
    row: Rect,
    hidden: usize,
    hovered: bool,
    palette: &Palette,
) {
    let mut text = StackStr::<24>::new();
    if hidden == 0 {
        let _ = text.write_str(SHOW_FEWER);
    } else {
        let _ = write!(text, "+{hidden} more");
    }
    if hovered {
        hover_band(buffer, row, palette);
    }
    put_truncated(
        buffer,
        row.x + 5,
        row.y,
        row.width.saturating_sub(6),
        text.as_str(),
        Style::default().fg(palette.overlay1),
    );
}

/// What `lines` entry lines show of `entries` entries: how many entries,
/// then the overflow row (`Some(hidden)` for `+N more`, `Some(0)` for
/// `show fewer`).
pub(super) fn line_plan(entries: usize, expanded: bool, lines: u16) -> (usize, Option<usize>) {
    let lines = usize::from(lines);
    if lines == 0 {
        return (0, None);
    }
    if entries <= lines {
        // A spare line while expanded closes it again.
        return (entries, (expanded && lines > entries).then_some(0));
    }
    let shown = lines - 1;
    (shown, Some(if expanded { 0 } else { entries - shown }))
}

/// Draws the block into `rect` (already painted with the chrome background)
/// and registers its hits. The rect may be shorter than `active_block_rows`
/// asked for: the entries then fold into `+N more`.
#[allow(clippy::too_many_arguments)] // the block's inputs; a struct would only rename them
pub(super) fn render_active_block(
    buffer: &mut Buffer,
    rect: Rect,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    view: ActiveView,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    if rect.is_empty() || !view.enabled || model.active.is_empty() {
        return;
    }
    let palette = &config.palette;
    let hover = state.sidebar_hover;
    let Some((header, lines)) = section_frame(buffer, rect, view.folded, palette) else {
        return;
    };
    let right = if view.folded {
        let mut marks = HeaderMarks::default();
        for class in [
            CLASS_BLOCKED,
            CLASS_VOICE,
            CLASS_WORKING,
            CLASS_SUBAGENTS,
            CLASS_DONE,
        ] {
            if model.active_classes & (1 << class) != 0 {
                let (glyph, color) = class_mark(class, config);
                marks.push(glyph, color);
            }
        }
        HeaderRight::Marks(marks)
    } else {
        HeaderRight::Count(model.active.len())
    };
    render_section_header(
        buffer,
        header,
        ACTIVE_TITLE,
        view.folded,
        right,
        matches!(hover, Some(SidebarHover::ActiveHeader)),
        palette,
    );
    hits.sidebar_active_header = header;
    if lines == 0 {
        return;
    }
    let (shown, overflow) = line_plan(model.active.len(), view.expanded, lines);
    let visible = &model.active[..shown.min(model.active.len())];
    let columns = EntryColumns::for_tabs(
        visible.iter().map(|entry| entry.tab),
        snapshot,
        model,
        rect.width,
        false,
    );
    let mut deadline = hits.sidebar_clock_deadline;
    let first_y = header.bottom();
    for (offset, entry) in visible.iter().enumerate() {
        let Some(tab) = snapshot.tabs.get(entry.tab as usize) else {
            continue;
        };
        let row = Rect::new(rect.x, first_y + offset as u16, rect.width, 1);
        let hovered = matches!(hover, Some(SidebarHover::ActiveEntry(id)) if *id == tab.tab_id);
        let line = EntryLine {
            columns,
            hovered,
            now: state.now,
            teams: state.teams,
        };
        if let Some(tick) = render_entry(buffer, row, snapshot, model, entry.tab, line, config) {
            deadline = Some(deadline.map_or(tick, |current| current.min(tick)));
        }
        hits.sidebar_active_rows.push((row, tab.tab_id.clone()));
    }
    hits.sidebar_clock_deadline = deadline;
    if let Some(hidden) = overflow {
        let row = Rect::new(rect.x, first_y + visible.len() as u16, rect.width, 1);
        let hovered = matches!(hover, Some(SidebarHover::ActiveMore));
        render_overflow_row(buffer, row, hidden, hovered, palette);
        hits.sidebar_active_more = row;
    }
}

/// A section header's right side: the entry count (open) or up to six
/// marks (folded), at stride 2 ending one cell before the margin.
pub(super) enum HeaderRight<'m> {
    Count(usize),
    Marks(HeaderMarks<'m>),
}

/// A folded header's marks, on the stack.
#[derive(Debug, Clone, Copy)]
pub(super) struct HeaderMarks<'m> {
    items: [(&'m str, Color); 6],
    len: usize,
}

impl Default for HeaderMarks<'_> {
    fn default() -> Self {
        Self {
            items: [("", Color::Reset); 6],
            len: 0,
        }
    }
}

impl<'m> HeaderMarks<'m> {
    pub(super) fn push(&mut self, glyph: &'m str, color: Color) {
        if let Some(slot) = self.items.get_mut(self.len) {
            *slot = (glyph, color);
            self.len += 1;
        }
    }

    fn as_slice(&self) -> &[(&'m str, Color)] {
        &self.items[..self.len]
    }
}

/// Whether a tab shows its time in state: working, blocked and finished
/// tabs, and an idle one running subagents (its time since it went idle);
/// an idle tab listed only for its voice session does not.
pub(super) fn shows_time(status: AgentStatus, subagents: u32) -> bool {
    match status {
        AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done => true,
        AgentStatus::Idle => subagents > 0,
        _ => false,
    }
}

/// [`shows_time`], plus every idle tab when `idle_time` (pinned entries).
fn entry_shows_time(status: AgentStatus, subagents: u32, idle_time: bool) -> bool {
    shows_time(status, subagents) || (idle_time && status == AgentStatus::Idle)
}

/// The hairline rule: `─` from x = 1 to `width - 2`, in surface1.
pub(super) fn put_rule(buffer: &mut Buffer, x: u16, y: u16, width: u16, palette: &Palette) {
    let style = Style::default().fg(palette.surface1);
    for column in 1..width.saturating_sub(1) {
        put_text(buffer, x + column, y, 1, "\u{2500}", style);
    }
}

/// The hover affordance: the row's band and the accent bar at x = 0.
pub(super) fn hover_band(buffer: &mut Buffer, row: Rect, palette: &Palette) {
    buffer.set_style(row, Style::default().bg(palette.sidebar_hover_bg()));
    put_text(
        buffer,
        row.x,
        row.y,
        1,
        HOVER_BAR,
        Style::default().fg(palette.accent),
    );
}

/// A section header: ` ▾ <title> … <count> ` (open) or ` ▸ <title> … <marks> `
/// (folded); the hovered header has the bar and a brighter title.
pub(super) fn render_section_header(
    buffer: &mut Buffer,
    row: Rect,
    title: &str,
    folded: bool,
    right: HeaderRight<'_>,
    hovered: bool,
    palette: &Palette,
) {
    let cw = row.width;
    if hovered {
        put_text(
            buffer,
            row.x,
            row.y,
            1,
            HOVER_BAR,
            Style::default().fg(palette.accent),
        );
    }
    let fold = if folded { "\u{25B8}" } else { "\u{25BE}" }; // ▸ ▾
    put_text(
        buffer,
        row.x + 1,
        row.y,
        1,
        fold,
        Style::default().fg(palette.overlay1),
    );
    // The right side: the entry count (open) or the marks (folded); the
    // title keeps what is left.
    let right_x = match right {
        HeaderRight::Marks(marks) => {
            let marks = marks.as_slice();
            if marks.is_empty() {
                cw.saturating_sub(1)
            } else {
                let present = marks.len() as u16;
                let first = cw
                    .saturating_sub(2)
                    .saturating_sub(present.saturating_sub(1) * 2);
                let mut x = first;
                for (glyph, color) in marks {
                    put_text(
                        buffer,
                        row.x + x,
                        row.y,
                        1,
                        glyph,
                        Style::default().fg(*color),
                    );
                    x += 2;
                }
                first
            }
        }
        HeaderRight::Count(entries) => {
            let mut count = StackStr::<8>::new();
            let _ = write!(count, "{}", entries.min(9_999_999));
            let width = display_width(count.as_str());
            // The count's last cell is cw - 4.
            let x = cw.saturating_sub(3).saturating_sub(width);
            put_text(
                buffer,
                row.x + x,
                row.y,
                width,
                count.as_str(),
                Style::default().fg(palette.overlay0),
            );
            x
        }
    };
    let title_style = Style::default()
        .fg(if hovered {
            palette.text
        } else {
            palette.subtext0
        })
        .add_modifier(Modifier::BOLD);
    put_truncated(
        buffer,
        row.x + 3,
        row.y,
        right_x.saturating_sub(4),
        title,
        title_style,
    );
}

/// The folded header's mark of a class: its status icon in its colour, the
/// live mic for voice, the subagent icon in the idle colour (as an idle
/// tab row draws it) for idle tabs running subagents.
pub(super) fn class_mark(class: u8, config: &ClientShellConfig) -> (&str, ratatui::style::Color) {
    let status = match class {
        CLASS_BLOCKED => AgentStatus::Blocked,
        CLASS_VOICE => return super::voice::voice_mark(AgentVoiceMode::Live, config),
        CLASS_WORKING => AgentStatus::Working,
        CLASS_SUBAGENTS => {
            return (
                super::tab_sidebar::TAB_SUBAGENTS_ICON,
                status_color(AgentStatus::Idle, &config.palette),
            )
        }
        _ => AgentStatus::Done,
    };
    (
        status_icon(status, config.status_indicators),
        status_color(status, &config.palette),
    )
}

/// The right-hand columns of the drawn entries (shared by every line).
#[derive(Debug, Clone, Copy)]
pub(super) struct EntryColumns {
    /// The time column's x (`TIME_CELLS` wide), when some drawn entry has a time.
    time: Option<u16>,
    /// The subagent column's x, when some drawn entry has subagents.
    subagents: Option<u16>,
    /// The name ends before this x.
    name_end: u16,
    /// An idle entry shows its time since it went idle too (the Pinned
    /// block; the Active block lists an idle tab only for its voice).
    idle_time: bool,
}

impl EntryColumns {
    /// The columns of the entry lines of `visible` (indices into
    /// `snapshot.tabs`). `idle_time` also times idle entries.
    pub(super) fn for_tabs(
        mut visible: impl Iterator<Item = u32> + Clone,
        snapshot: &ClientShellSnapshot,
        model: &SidebarModel,
        width: u16,
        idle_time: bool,
    ) -> Self {
        let has_time = visible.clone().any(|index| {
            snapshot
                .tabs
                .get(index as usize)
                .zip(model.tab(index))
                .is_some_and(|(tab, facts)| {
                    entry_shows_time(tab.agent_status, facts.subagents, idle_time)
                        && facts.since.is_some()
                })
        });
        let has_subagents =
            visible.any(|index| model.tab(index).is_some_and(|facts| facts.subagents > 0));
        // The last cell is the margin.
        let mut right = width.saturating_sub(1);
        let time = has_time.then(|| {
            right = right.saturating_sub(TIME_CELLS);
            right
        });
        let subagents = has_subagents.then(|| {
            right = right.saturating_sub(SUBAGENT_CELLS);
            right
        });
        Self {
            time,
            subagents,
            name_end: right.saturating_sub(1),
            idle_time,
        }
    }
}

/// One entry line's shared inputs.
#[derive(Clone, Copy)]
pub(super) struct EntryLine<'t> {
    pub(super) columns: EntryColumns,
    pub(super) hovered: bool,
    pub(super) now: std::time::Instant,
    /// The active endpoint's teams (a team group's entry names the team).
    pub(super) teams: Option<&'t super::teams::ClientTeamsState>,
}

/// Draws one entry line; returns when its duration next changes its text.
pub(super) fn render_entry(
    buffer: &mut Buffer,
    row: Rect,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    tab_index: u32,
    line: EntryLine<'_>,
    config: &ClientShellConfig,
) -> Option<std::time::Instant> {
    let palette = &config.palette;
    let tab = snapshot.tabs.get(tab_index as usize)?;
    let facts = model.tab(tab_index)?;
    if line.hovered {
        hover_band(buffer, row, palette);
    }
    let status = tab.agent_status;
    let status_fg = status_color(status, palette);
    let icon =
        super::sidebar_model::tab_row_icon(status, facts.subagents, config.status_indicators);
    put_text(
        buffer,
        row.x + 3,
        row.y,
        1,
        icon,
        Style::default().fg(status_fg),
    );
    let mut name_x = 5;
    if let Some(voice) = facts.voice {
        let (mark, color) = super::voice::voice_mark(voice, config);
        put_text(
            buffer,
            row.x + 5,
            row.y,
            1,
            mark,
            Style::default().fg(color),
        );
        name_x = 7;
    }
    let mut tick = None;
    if let (Some(x), Some(since)) = (
        line.columns.time,
        facts
            .since
            .filter(|_| entry_shows_time(status, facts.subagents, line.columns.idle_time)),
    ) {
        let age = format_age(line.now.saturating_duration_since(since));
        let width = display_width(age.as_str()).min(TIME_CELLS);
        let fg = if status == AgentStatus::Blocked {
            palette.red
        } else {
            palette.overlay1
        };
        put_text(
            buffer,
            row.x + x + (TIME_CELLS - width),
            row.y,
            width,
            age.as_str(),
            Style::default().fg(fg),
        );
        tick = Some(next_age_tick(since, line.now));
    }
    if let Some(x) = line.columns.subagents.filter(|_| facts.subagents > 0) {
        let mut count = StackStr::<16>::new();
        let _ = write!(
            count,
            "{}{}",
            super::tab_sidebar::TAB_SUBAGENTS_ICON,
            facts.subagents.min(99)
        );
        put_text(
            buffer,
            row.x + x,
            row.y,
            SUBAGENT_CELLS - 1,
            count.as_str(),
            Style::default().fg(status_fg),
        );
    }
    let name_width = line.columns.name_end.saturating_sub(name_x);
    let name_fg = super::tab_color::tab_label_fg(tab.color, palette).unwrap_or(palette.text);
    let mut name_style = Style::default().fg(name_fg);
    if status == AgentStatus::Blocked {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    put_truncated(
        buffer,
        row.x + name_x,
        row.y,
        name_width,
        &tab.label,
        name_style,
    );
    // The group after the name, while at least 3 cells remain.
    let used = display_width(&tab.label).min(name_width);
    let room = name_width.saturating_sub(used + 1);
    put_group_text(
        buffer,
        row.x + name_x + used + 1,
        row.y,
        room,
        snapshot,
        facts,
        line.teams,
        palette,
    );
    tick
}

/// A tab's group after its name in `room` cells (nothing under 3): a team
/// group as `◆` (accent) and its purpose (overlay1; the label before it has
/// one), a plain group as its label (overlay0), nothing when ungrouped.
#[allow(clippy::too_many_arguments)] // a cell run's inputs; a struct would only rename them
pub(super) fn put_group_text(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    room: u16,
    snapshot: &ClientShellSnapshot,
    facts: &super::sidebar_model::TabFacts,
    teams: Option<&super::teams::ClientTeamsState>,
    palette: &Palette,
) {
    if room < 3 {
        return;
    }
    let Some(workspace) = facts
        .workspace
        .filter(|index| super::tab_sidebar::is_group_index(*index as usize))
        .and_then(|index| snapshot.workspaces.get(index as usize))
    else {
        return;
    };
    match teams.and_then(|teams| teams.team(&workspace.workspace_id)) {
        Some(team) => {
            let (name, _) = super::teams::header_label(team, &workspace.label);
            put_text(
                buffer,
                x,
                y,
                1,
                super::teams::TEAM_MARK,
                Style::default().fg(palette.accent),
            );
            put_truncated(
                buffer,
                x + 2,
                y,
                room - 2,
                name,
                Style::default().fg(palette.overlay1),
            );
        }
        None => put_truncated(
            buffer,
            x,
            y,
            room,
            &workspace.label,
            Style::default().fg(palette.overlay0),
        ),
    }
}

/// The hover target under the pointer, borrowed from the hit map so an
/// unchanged target is compared without allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoverAt<'a> {
    Tab(&'a str),
    Group(&'a str),
    ActiveHeader,
    ActiveEntry(&'a str),
    ActiveMore,
    Fixed(FixedKind),
    Run(&'a str),
    PinsHeader,
    PinsEntry(&'a str),
    PinsMore,
    ScheduledHeader,
    ScheduledEntry(&'a str),
    ScheduledMore,
}

impl HoverAt<'_> {
    fn is(self, hover: &SidebarHover) -> bool {
        match (self, hover) {
            (Self::Tab(id), SidebarHover::Tab(current))
            | (Self::Group(id), SidebarHover::Group(current))
            | (Self::ActiveEntry(id), SidebarHover::ActiveEntry(current))
            | (Self::PinsEntry(id), SidebarHover::PinsEntry(current))
            | (Self::ScheduledEntry(id), SidebarHover::ScheduledEntry(current))
            | (Self::Run(id), SidebarHover::Run(current)) => id == current,
            (Self::ActiveHeader, SidebarHover::ActiveHeader)
            | (Self::ActiveMore, SidebarHover::ActiveMore)
            | (Self::PinsHeader, SidebarHover::PinsHeader)
            | (Self::PinsMore, SidebarHover::PinsMore)
            | (Self::ScheduledHeader, SidebarHover::ScheduledHeader)
            | (Self::ScheduledMore, SidebarHover::ScheduledMore) => true,
            (Self::Fixed(kind), SidebarHover::Fixed(current)) => kind == *current,
            _ => false,
        }
    }

    fn to_owned(self) -> SidebarHover {
        match self {
            Self::Tab(id) => SidebarHover::Tab(id.to_owned()),
            Self::Group(id) => SidebarHover::Group(id.to_owned()),
            Self::ActiveHeader => SidebarHover::ActiveHeader,
            Self::ActiveEntry(id) => SidebarHover::ActiveEntry(id.to_owned()),
            Self::ActiveMore => SidebarHover::ActiveMore,
            Self::Fixed(kind) => SidebarHover::Fixed(kind),
            Self::Run(id) => SidebarHover::Run(id.to_owned()),
            Self::PinsHeader => SidebarHover::PinsHeader,
            Self::PinsEntry(id) => SidebarHover::PinsEntry(id.to_owned()),
            Self::PinsMore => SidebarHover::PinsMore,
            Self::ScheduledHeader => SidebarHover::ScheduledHeader,
            Self::ScheduledEntry(id) => SidebarHover::ScheduledEntry(id.to_owned()),
            Self::ScheduledMore => SidebarHover::ScheduledMore,
        }
    }
}

/// The input side of the block and the sidebar hover (`mouse.rs` calls in).
impl ClientShellState {
    /// Whether `point` is inside the expanded tabs sidebar (the divider
    /// column bounds it on the right).
    fn in_tabs_sidebar(&self, point: (u16, u16)) -> bool {
        let divider = self.hits.sidebar_divider;
        !self.sidebar_collapsed
            && self.config.sidebar_layout == crate::config::SidebarLayoutConfig::Tabs
            && divider.height > 0
            && point.0 <= divider.x
            && point.1 >= divider.y
            && point.1 < divider.bottom()
    }

    fn hover_at(&self, point: (u16, u16)) -> Option<HoverAt<'_>> {
        if !self.in_tabs_sidebar(point) {
            return None;
        }
        let hits = &self.hits;
        fn id_at(rows: &[(Rect, String)], point: (u16, u16)) -> Option<&str> {
            rows.iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .map(|(_, id)| id.as_str())
        }
        if let Some(id) = id_at(&hits.sidebar_runs, point) {
            return Some(HoverAt::Run(id));
        }
        if super::contains(hits.sidebar_active_header, point) {
            return Some(HoverAt::ActiveHeader);
        }
        if let Some(id) = id_at(&hits.sidebar_active_rows, point) {
            return Some(HoverAt::ActiveEntry(id));
        }
        if super::contains(hits.sidebar_active_more, point) {
            return Some(HoverAt::ActiveMore);
        }
        if super::contains(hits.sidebar_pins_header, point) {
            return Some(HoverAt::PinsHeader);
        }
        if let Some(id) = id_at(&hits.sidebar_pins_rows, point) {
            return Some(HoverAt::PinsEntry(id));
        }
        if super::contains(hits.sidebar_pins_more, point) {
            return Some(HoverAt::PinsMore);
        }
        if super::contains(hits.sidebar_scheduled_header, point) {
            return Some(HoverAt::ScheduledHeader);
        }
        if let Some(id) = id_at(&hits.sidebar_scheduled_rows, point) {
            return Some(HoverAt::ScheduledEntry(id));
        }
        if super::contains(hits.sidebar_scheduled_more, point) {
            return Some(HoverAt::ScheduledMore);
        }
        if let Some(id) = id_at(&hits.sidebar_tabs, point) {
            return Some(HoverAt::Tab(id));
        }
        if let Some(id) = id_at(&hits.sidebar_groups, point) {
            return Some(HoverAt::Group(id));
        }
        [
            (hits.browser_row, FixedKind::Browser),
            (hits.news_row, FixedKind::News),
            (hits.coordinator_row, FixedKind::Coordinator),
        ]
        .into_iter()
        .find(|(rect, _)| super::contains(*rect, point))
        .map(|(_, kind)| HoverAt::Fixed(kind))
    }

    /// Mouse motion: track the hovered tabs sidebar row; repaint only when
    /// the target changes (a point outside the sidebar, or over no row,
    /// clears it).
    pub(super) fn update_sidebar_hover(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) {
        let target = self.hover_at(point);
        let unchanged = match (target, self.sidebar_hover.as_ref()) {
            (None, None) => true,
            (Some(target), Some(current)) => target.is(current),
            _ => false,
        };
        if unchanged {
            return;
        }
        self.sidebar_hover = target.map(HoverAt::to_owned);
        outcome.repaint = true;
    }

    /// The tab id of the sidebar entry at `point` that opens its tab's menu
    /// on a right click: an Active, Pinned or Scheduled entry.
    pub(super) fn sidebar_active_entry_at(&self, point: (u16, u16)) -> Option<String> {
        let hits = &self.hits;
        [
            &hits.sidebar_active_rows,
            &hits.sidebar_pins_rows,
            &hits.sidebar_scheduled_rows,
        ]
        .into_iter()
        .flatten()
        .find(|(rect, _)| super::contains(*rect, point))
        .map(|(_, tab_id)| tab_id.clone())
    }

    /// A left press on the Active block: the header folds it, the
    /// overflow row expands or shrinks it, an entry jumps to its tab. Never
    /// starts a drag. Returns whether the press was the block's.
    pub(super) fn sidebar_active_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if super::contains(self.hits.sidebar_active_header, point) {
            self.active_agents_folded = Some(!self.active_agents_folded.unwrap_or(false));
            outcome.repaint = true;
            self.persist_chrome_preferences(outcome);
            return true;
        }
        if super::contains(self.hits.sidebar_active_more, point) {
            self.active_agents_expanded = !self.active_agents_expanded;
            outcome.repaint = true;
            return true;
        }
        let Some(tab_id) = self
            .hits
            .sidebar_active_rows
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, tab_id)| tab_id.clone())
        else {
            return false;
        };
        self.jump_to_sidebar_tab(tab_id, outcome);
        true
    }

    /// Fork: a left press on a suspended run's row expands it, or folds it
    /// again (persisted). Never starts a drag. Returns whether the press was
    /// a run row's.
    pub(super) fn sidebar_run_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(tab_id) = self
            .hits
            .sidebar_runs
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, tab_id)| tab_id.clone())
        else {
            return false;
        };
        if !self.expanded_runs.remove(&tab_id) {
            self.expanded_runs.insert(tab_id);
        }
        self.sidebar_model.mark_dirty();
        self.persist_chrome_preferences(outcome);
        outcome.repaint = true;
        true
    }

    /// Unfold `workspace_id`'s group (persisted when it was folded).
    fn open_sidebar_group(&mut self, workspace_id: &str, outcome: &mut ClientShellInput) {
        if self.collapsed_groups.remove(&group_key(workspace_id)) {
            self.persist_chrome_preferences(outcome);
        }
        // The list rows follow fold state.
        self.sidebar_model.mark_dirty();
    }

    /// Focus `tab_id`, open its group and scroll the list to it (before the
    /// server's focus change lands). Shared by the Active, Pinned and
    /// Scheduled entries.
    pub(super) fn jump_to_sidebar_tab(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
        let workspace_id = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .tabs
                .iter()
                .find(|tab| tab.tab_id == tab_id)
                .map(|tab| tab.workspace_id.clone())
        });
        if let Some(workspace_id) = workspace_id {
            self.open_sidebar_group(&workspace_id, outcome);
        }
        self.push_endpoint_method(
            crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                tab_id: tab_id.clone(),
            }),
            outcome,
        );
        self.sidebar_reveal_tab = Some(tab_id);
        outcome.repaint = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_fold_into_more_or_show_fewer() {
        assert_eq!(line_plan(3, false, 4), (3, None));
        assert_eq!(line_plan(9, false, 4), (3, Some(6)));
        assert_eq!(line_plan(9, true, 4), (3, Some(0)));
        assert_eq!(line_plan(9, true, 10), (9, Some(0)));
        assert_eq!(line_plan(3, true, 3), (3, None));
        assert_eq!(line_plan(3, false, 0), (0, None));
        assert_eq!(line_plan(3, false, 1), (0, Some(3)));
    }

    #[test]
    fn caps_step_down_with_height() {
        assert_eq!(active_cap(45), 8);
        assert_eq!(active_cap(30), 6);
        assert_eq!(active_cap(20), 4);
    }
}
