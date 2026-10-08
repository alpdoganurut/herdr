//! The tabs sidebar's detail strip (fork, sidebar v2): facts about the
//! selected row (`sidebar_model::resolve_selected`: the hovered row, else
//! the focused tab) between the pinned rows and the status footer.
//!
//! The strip is framed by hairline rules: one on its first row, one on its
//! last row while the status footer shows under it. The text rows hold
//! "chips" separated by two spaces that wrap to the next row; when the last
//! row overflows its last cell becomes `…`. Line 1 names the subject, the
//! rest lists only the facts that apply. Everything is written straight into
//! the buffer from snapshot borrows and stack strings (no `format!`), and
//! only the one subject is looked up, never a row per frame.

use std::fmt::Write as _;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::render::{display_width, put_text, put_truncated, ShellRenderState};
use super::sidebar_model::{
    format_age, next_age_tick, resolve_selected, FixedKind, Row, Selected, SidebarModel, StackStr,
    CLASS_BLOCKED, CLASS_DONE, CLASS_SUBAGENTS, CLASS_VOICE, CLASS_WORKING,
};
use super::*;
use crate::api::schema::{AgentStatus, AgentVoiceMode, TabColor, TabRemindInterval};

/// The active header subject's order hint.
const ACTIVE_ORDER_HINT: &str = "blocked > voice > working > subagents > done";

/// The strip's text rows at a sidebar content height (0: no strip).
pub(super) fn detail_lines(content_height: u16) -> u16 {
    if content_height >= 36 {
        3
    } else if content_height >= 28 {
        2
    } else if content_height >= 20 {
        1
    } else {
        0
    }
}

/// Draws the strip into `rect` (already painted with the chrome background)
/// and registers its hit. Fork (sidebar v3): the strip sits under the
/// toolbar; the rect is the text rows, then the rule that separates them
/// from the list.
pub(super) fn render_detail_strip(
    buffer: &mut Buffer,
    rect: Rect,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    if rect.is_empty() {
        return;
    }
    let palette = &config.palette;
    hits.sidebar_detail = rect;
    super::tab_sidebar_active::put_rule(buffer, rect.x, rect.bottom() - 1, rect.width, palette);
    let rows = rect.height - 1;
    if rows == 0 {
        return;
    }
    let mut chips = Chips {
        buffer,
        x0: rect.x + 2,
        y0: rect.y,
        width: rect.width.saturating_sub(3),
        rows,
        line: 0,
        x: 0,
        overflow: false,
    };
    let selected = match resolve_selected(snapshot, model, state.sidebar_hover) {
        // The block is gone (nothing active, pinned or scheduled): describe
        // the focused tab.
        Selected::ActiveHeader if model.active.is_empty() => {
            model.focused_tab.map_or(Selected::None, Selected::Tab)
        }
        Selected::PinsHeader if model.pins.is_empty() => {
            model.focused_tab.map_or(Selected::None, Selected::Tab)
        }
        Selected::ScheduledHeader if model.scheduled.is_empty() => {
            model.focused_tab.map_or(Selected::None, Selected::Tab)
        }
        selected => selected,
    };
    match selected {
        Selected::Tab(index)
        | Selected::ActiveEntry(index)
        | Selected::PinsEntry(index)
        | Selected::ScheduledEntry(index) => {
            if let Some(tick) = tab_chips(&mut chips, snapshot, model, index, config, state) {
                hits.sidebar_clock_deadline = Some(
                    hits.sidebar_clock_deadline
                        .map_or(tick, |current| current.min(tick)),
                );
            }
        }
        Selected::Group(index) => group_chips(&mut chips, snapshot, model, index, config, state),
        Selected::ActiveHeader => active_chips(&mut chips, model, config),
        Selected::PinsHeader => pins_chips(&mut chips, model, config),
        Selected::ScheduledHeader => {
            if let Some(tick) = scheduled_chips(&mut chips, snapshot, model, config, state) {
                hits.sidebar_clock_deadline = Some(
                    hits.sidebar_clock_deadline
                        .map_or(tick, |current| current.min(tick)),
                );
            }
        }
        Selected::Fixed(kind) => fixed_chips(&mut chips, kind, config, state),
        Selected::Run { start, len } => run_chips(&mut chips, snapshot, model, start, len, config),
        Selected::None => {}
    }
    chips.finish(palette);
}

/// Chips written into the strip's text rows.
struct Chips<'b> {
    buffer: &'b mut Buffer,
    x0: u16,
    y0: u16,
    width: u16,
    rows: u16,
    line: u16,
    x: u16,
    overflow: bool,
}

impl Chips<'_> {
    /// One chip of adjacent parts, after a two-space gap; wraps to the next
    /// row when it does not fit (a chip wider than a row is truncated).
    fn chip(&mut self, parts: &[(&str, Style)]) {
        if self.overflow || self.width == 0 {
            return;
        }
        let width: u16 = parts
            .iter()
            .map(|(text, _)| display_width(text))
            .fold(0, u16::saturating_add);
        if width == 0 {
            return;
        }
        if self.x > 0 {
            if self.x.saturating_add(2).saturating_add(width) <= self.width {
                self.x += 2;
            } else {
                self.newline();
            }
        }
        if self.line >= self.rows {
            self.overflow = true;
            return;
        }
        let y = self.y0 + self.line;
        for (text, style) in parts {
            let room = self.width.saturating_sub(self.x);
            if room == 0 {
                break;
            }
            let cells = display_width(text);
            if cells > room {
                put_truncated(self.buffer, self.x0 + self.x, y, room, text, *style);
                self.x = self.width;
                break;
            }
            put_text(self.buffer, self.x0 + self.x, y, cells, text, *style);
            self.x += cells;
        }
    }

    /// Ends the current row (nothing while it is empty).
    fn newline(&mut self) {
        if self.x == 0 {
            return;
        }
        self.line += 1;
        self.x = 0;
    }

    /// Marks an overflow with `…` in the last row's last cell.
    fn finish(self, palette: &Palette) {
        if self.overflow && self.rows > 0 && self.width > 0 {
            put_text(
                self.buffer,
                self.x0 + self.width - 1,
                self.y0 + self.rows - 1,
                1,
                "\u{2026}",
                Style::default().fg(palette.overlay0),
            );
        }
    }
}

fn fg(color: ratatui::style::Color) -> Style {
    Style::default().fg(color)
}

/// A tab (list row or Active entry): returns when its shown duration next
/// changes its text.
fn tab_chips(
    chips: &mut Chips<'_>,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    index: u32,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
) -> Option<std::time::Instant> {
    let palette = &config.palette;
    let tab = snapshot.tabs.get(index as usize)?;
    let facts = model.tab(index)?;
    let label_fg = super::tab_color::tab_label_fg(tab.color, palette).unwrap_or(palette.text);
    chips.chip(&[(&tab.label, fg(label_fg).add_modifier(Modifier::BOLD))]);
    if let Some(workspace) = facts
        .workspace
        .filter(|index| super::tab_sidebar::is_group_index(*index as usize))
        .and_then(|index| snapshot.workspaces.get(index as usize))
    {
        chips.chip(&[(&workspace.label, fg(palette.overlay0))]);
    }
    chips.newline();

    let status = tab.agent_status;
    let status_style = fg(status_color(status, palette));
    let icon =
        super::sidebar_model::tab_row_icon(status, facts.subagents, config.status_indicators);
    let parked = facts.parked && status != AgentStatus::Suspended;
    chips.chip(&[
        (icon, status_style),
        (" ", status_style),
        (status_text(status), status_style),
        (if parked { " (suspended)" } else { "" }, status_style),
    ]);
    let mut tick = None;
    if super::tab_sidebar_active::shows_time(status, facts.subagents) {
        if let Some(since) = facts.since {
            let age = format_age(state.now.saturating_duration_since(since));
            chips.chip(&[(age.as_str(), fg(palette.overlay1))]);
            tick = Some(next_age_tick(since, state.now));
        }
    }
    if facts.subagents > 0 {
        let mut count = StackStr::<12>::new();
        let _ = write!(count, "{}", facts.subagents);
        chips.chip(&[
            (super::tab_sidebar::TAB_SUBAGENTS_ICON, status_style),
            (count.as_str(), status_style),
        ]);
    }
    if let Some(agent) = facts
        .primary_agent
        .and_then(|agent| snapshot.agents.get(agent as usize))
    {
        let key = agent.agent.as_deref().unwrap_or("other");
        let glyph = crate::config::tab_agent_glyph(&config.tab_agent_glyphs, key);
        let color = crate::config::tab_agent_glyph_color(&config.tab_agent_glyph_colors, key)
            .unwrap_or(palette.overlay1);
        let kind = agent
            .display_agent
            .as_deref()
            .or(agent.agent.as_deref())
            .unwrap_or("agent");
        chips.chip(&[
            (glyph, fg(color)),
            (if glyph.is_empty() { "" } else { " " }, fg(color)),
            (kind, fg(color)),
        ]);
        if let Some(name) = agent
            .name
            .as_deref()
            .filter(|name| !name.is_empty() && *name != tab.label)
        {
            chips.chip(&[(name, fg(palette.overlay1))]);
        }
    }
    // Fork: the agents' context use, at any level.
    if let Some(usage) = facts.context {
        let text = context_chip_text(usage);
        let color = if usage.percent() >= super::agent_context::ROW_WARN_PERCENT {
            super::agent_context::row_percent_color(usage, palette)
        } else {
            palette.overlay1
        };
        chips.chip(&[(text.as_str(), fg(color))]);
    }
    if facts.panes > 1 {
        let mut count = StackStr::<12>::new();
        let _ = write!(count, "{}", facts.panes);
        chips.chip(&[
            (count.as_str(), fg(palette.overlay1)),
            (" panes", fg(palette.overlay1)),
        ]);
    }
    // The member's role: an O(1) membership check, then one scan of the
    // subject's team, once per frame.
    if let Some(member) = state
        .teams
        .filter(|teams| teams.is_member_tab(&tab.tab_id))
        .and_then(|teams| teams.team(&tab.workspace_id))
        .and_then(|team| {
            team.members
                .iter()
                .find(|member| member.tab_id.as_deref() == Some(tab.tab_id.as_str()))
        })
    {
        let role = member
            .role
            .as_deref()
            .filter(|role| !role.is_empty())
            .unwrap_or("member");
        chips.chip(&[
            (super::teams::TEAM_MARK, fg(palette.overlay1)),
            (" ", fg(palette.overlay1)),
            (role, fg(palette.overlay1)),
        ]);
    }
    if tab.important || tab.remind_every.is_some() {
        let key = (state.active_endpoint_id.clone(), tab.tab_id.clone());
        if tab.important {
            let lit = state
                .idle_reminders
                .get(&key)
                .and_then(|reminder| reminder.lit);
            let color = lit.map_or(palette.overlay1, |lit| lit.color(palette));
            chips.chip(&[(super::tab_sidebar::TAB_IMPORTANT_MARKER, fg(color))]);
        }
        if let Some(every) = tab
            .remind_every
            .filter(|every| *every != TabRemindInterval::Unknown)
        {
            chips.chip(&[
                (
                    super::tab_sidebar::remind_marker(every),
                    fg(palette.overlay1),
                ),
                (" ", fg(palette.overlay1)),
                (every.name(), fg(palette.overlay1)),
            ]);
        }
    }
    if state.browser_marked_tabs.contains(&tab.tab_id) {
        chips.chip(&[(super::browser::TAB_BROWSER_MARKER, fg(palette.accent))]);
    }
    // Fork (sidebar v3): pinned from the tab menu.
    if facts.pin {
        chips.chip(&[("pinned", fg(palette.overlay1))]);
    }
    // Fork: notifications muted from the tab menu.
    if facts.muted {
        chips.chip(&[("notifications off", fg(palette.overlay1))]);
    }
    if let Some(voice) = facts.voice {
        let (mark, color) = super::voice::voice_mark(voice, config);
        let word = if voice == AgentVoiceMode::Muted {
            "muted"
        } else {
            "live"
        };
        chips.chip(&[(mark, fg(color)), (" ", fg(color)), (word, fg(color))]);
    }
    if let Some(color) = tab.color.filter(|color| *color != TabColor::Unknown) {
        let tag_fg = super::tab_color::tab_label_fg(Some(color), palette).unwrap_or(palette.text);
        chips.chip(&[(color.name(), fg(tag_fg))]);
    }
    chips.chip(&[(&tab.tab_id, fg(palette.overlay0))]);
    tick
}

/// Fork: `ctx 82% · 164k/200k`.
pub(super) fn context_chip_text(usage: crate::agent_context::ContextUsage) -> StackStr<40> {
    let mut text = StackStr::new();
    let _ = write!(text, "ctx {}% \u{b7} ", usage.percent());
    write_tokens(&mut text, usage.used);
    let _ = text.write_char('/');
    write_tokens(&mut text, usage.window);
    text
}

/// Tokens as `950`, `164k`, `1M`, `1.2M`.
fn write_tokens(text: &mut StackStr<40>, tokens: u64) {
    let _ = if tokens >= 1_000_000 {
        let tenths = tokens / 100_000;
        if tenths.is_multiple_of(10) {
            write!(text, "{}M", tenths / 10)
        } else {
            write!(text, "{}.{}M", tenths / 10, tenths % 10)
        }
    } else if tokens >= 1_000 {
        write!(text, "{}k", tokens / 1_000)
    } else {
        write!(text, "{tokens}")
    };
}

/// The status order of the per-status counts.
const COUNTED: [AgentStatus; 6] = [
    AgentStatus::Blocked,
    AgentStatus::Working,
    AgentStatus::Done,
    AgentStatus::Idle,
    AgentStatus::Suspended,
    AgentStatus::Unknown,
];

fn count_chip(chips: &mut Chips<'_>, icon: &str, count: usize, word: &str, style: Style) {
    let mut number = StackStr::<12>::new();
    let _ = write!(number, "{count}");
    chips.chip(&[
        (icon, style),
        (" ", style),
        (number.as_str(), style),
        (" ", style),
        (word, style),
    ]);
}

/// A group header: its name, the team, per-status counts, then its id, size
/// and fold state.
fn group_chips(
    chips: &mut Chips<'_>,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    index: u32,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
) {
    let palette = &config.palette;
    let Some(workspace) = snapshot.workspaces.get(index as usize) else {
        return;
    };
    chips.chip(&[(
        &workspace.label,
        fg(palette.text).add_modifier(Modifier::BOLD),
    )]);
    chips.newline();
    if let Some(team) = state
        .teams
        .and_then(|teams| teams.team(&workspace.workspace_id))
    {
        let purpose = team
            .purpose
            .as_deref()
            .map(str::trim)
            .filter(|purpose| !purpose.is_empty());
        let mark = fg(if purpose.is_some() {
            palette.accent
        } else {
            palette.overlay0
        });
        chips.chip(&[(super::teams::TEAM_MARK, mark), (" team", mark)]);
        if let Some(purpose) = purpose {
            chips.chip(&[(purpose, fg(palette.overlay1))]);
        }
    }
    // Per-status counts over the group's listed tabs: one pass, no allocation.
    let mut counts = [0usize; COUNTED.len()];
    for (tab, facts) in snapshot.tabs.iter().zip(&model.tabs) {
        if facts.fixed || facts.workspace != Some(index) {
            continue;
        }
        if let Some(slot) = COUNTED
            .iter()
            .position(|status| *status == tab.agent_status)
        {
            counts[slot] += 1;
        }
    }
    for (status, count) in COUNTED.iter().zip(counts) {
        if count > 0 {
            count_chip(
                chips,
                status_icon(*status, config.status_indicators),
                count,
                status_text(*status),
                fg(status_color(*status, palette)),
            );
        }
    }
    chips.newline();
    let (members, folded) = model
        .rows
        .iter()
        .find_map(|row| match row {
            Row::Header {
                workspace,
                folded,
                members,
            } if *workspace == index => Some((usize::from(*members), *folded)),
            _ => None,
        })
        .unwrap_or_default();
    let dim = fg(palette.overlay0);
    chips.chip(&[(&workspace.workspace_id, dim)]);
    let mut size = StackStr::<16>::new();
    let _ = write!(size, "{members} tabs");
    chips.chip(&[(size.as_str(), dim)]);
    chips.chip(&[(if folded { "folded" } else { "open" }, dim)]);
}

/// The Active header (or its `+N more` row): counts per class and
/// the order.
fn active_chips(chips: &mut Chips<'_>, model: &SidebarModel, config: &ClientShellConfig) {
    let palette = &config.palette;
    chips.chip(&[(
        super::tab_sidebar_active::ACTIVE_TITLE,
        fg(palette.text).add_modifier(Modifier::BOLD),
    )]);
    chips.newline();
    for (class, word) in [
        (CLASS_BLOCKED, "blocked"),
        (CLASS_VOICE, "voice"),
        (CLASS_WORKING, "working"),
        (CLASS_SUBAGENTS, "subagents"),
        (CLASS_DONE, "done"),
    ] {
        let count = model
            .active
            .iter()
            .filter(|entry| entry.class == class)
            .count();
        if count > 0 {
            let (icon, color) = super::tab_sidebar_active::class_mark(class, config);
            count_chip(chips, icon, count, word, fg(color));
        }
    }
    chips.newline();
    chips.chip(&[(ACTIVE_ORDER_HINT, fg(palette.overlay0))]);
}

/// Fork (sidebar v3): the Pinned header (or its overflow row): the count
/// and how to pin.
/// Fork: a suspended run's row: the count, then its tabs' names (as many
/// as fit).
fn run_chips(
    chips: &mut Chips<'_>,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    start: u32,
    len: u16,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let mut count = StackStr::<24>::new();
    let _ = write!(count, "{len} suspended");
    chips.chip(&[(
        count.as_str(),
        fg(palette.text).add_modifier(Modifier::BOLD),
    )]);
    chips.newline();
    for tab in model.run(start, len) {
        if let Some(tab) = snapshot.tabs.get(*tab as usize) {
            chips.chip(&[(tab.label.as_str(), fg(palette.overlay1))]);
        }
    }
}

fn pins_chips(chips: &mut Chips<'_>, model: &SidebarModel, config: &ClientShellConfig) {
    let palette = &config.palette;
    chips.chip(&[(
        super::tab_sidebar_pins::PINS_TITLE,
        fg(palette.text).add_modifier(Modifier::BOLD),
    )]);
    chips.newline();
    let mut count = StackStr::<16>::new();
    let _ = write!(count, "{} pinned", model.pins.len());
    chips.chip(&[(count.as_str(), fg(palette.overlay1))]);
    chips.newline();
    chips.chip(&[("right-click a tab \u{2192} Pin", fg(palette.overlay0))]);
}

/// Fork (sidebar v3): the Scheduled header (or its overflow row): the
/// count and the soonest countdown; returns when that countdown next
/// changes its text.
fn scheduled_chips(
    chips: &mut Chips<'_>,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
) -> Option<std::time::Instant> {
    let palette = &config.palette;
    chips.chip(&[(
        super::tab_sidebar_scheduled::SCHEDULED_TITLE,
        fg(palette.text).add_modifier(Modifier::BOLD),
    )]);
    chips.newline();
    let mut count = StackStr::<16>::new();
    let _ = write!(count, "{} scheduled", model.scheduled.len());
    chips.chip(&[(count.as_str(), fg(palette.overlay1))]);
    if let Some((index, next_fire)) =
        super::tab_sidebar_scheduled::soonest_countdown(snapshot, model, state)
    {
        if let Some(tab) = snapshot.tabs.get(index as usize) {
            let countdown = super::sidebar_model::format_countdown(
                next_fire.saturating_duration_since(state.now),
            );
            chips.chip(&[
                ("next ", fg(palette.overlay1)),
                (&tab.label, fg(palette.text)),
                (" ", fg(palette.overlay1)),
                (countdown.as_str(), fg(palette.subtext0)),
            ]);
            return super::sidebar_model::next_countdown_tick(next_fire, state.now);
        }
    }
    None
}

/// A pinned row: its label, glyph and status, and its tab.
fn fixed_chips(
    chips: &mut Chips<'_>,
    kind: FixedKind,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
) {
    let palette = &config.palette;
    let (label, glyph, color, status, tab_id) = match kind {
        FixedKind::Browser => {
            let Some(row) = state.browser_row.as_ref() else {
                return;
            };
            (
                super::browser::BROWSER_ROW_LABEL,
                row.state.glyph(),
                row.state.color(palette, row.active),
                row.status.as_str(),
                None,
            )
        }
        FixedKind::News => {
            let Some(row) = state.news_row.as_ref() else {
                return;
            };
            (
                super::news::NEWS_ROW_LABEL,
                row.state.glyph(),
                row.state.color(palette),
                row.status.as_str(),
                row.tab_id.as_deref(),
            )
        }
        FixedKind::Coordinator => {
            let Some(row) = state.coordinator_row.as_ref() else {
                return;
            };
            (
                super::coordinator::COORDINATOR_ROW_LABEL,
                row.state.glyph(),
                row.state.color(palette),
                row.status.as_str(),
                row.tab_id.as_deref(),
            )
        }
    };
    chips.chip(&[(label, fg(palette.text).add_modifier(Modifier::BOLD))]);
    chips.newline();
    chips.chip(&[(glyph, fg(color))]);
    chips.chip(&[(status, fg(palette.overlay1))]);
    if let Some(tab_id) = tab_id {
        chips.chip(&[(tab_id, fg(palette.overlay0))]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_lines_step_down_with_height() {
        assert_eq!(detail_lines(45), 3);
        assert_eq!(detail_lines(30), 2);
        assert_eq!(detail_lines(22), 1);
        assert_eq!(detail_lines(14), 0);
    }

    #[test]
    fn chips_wrap_and_mark_an_overflow() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 3));
        let palette = Palette::catppuccin();
        let mut chips = Chips {
            buffer: &mut buffer,
            x0: 0,
            y0: 0,
            width: 12,
            rows: 2,
            line: 0,
            x: 0,
            overflow: false,
        };
        let style = Style::default();
        chips.chip(&[("alpha", style)]);
        chips.chip(&[("beta", style)]);
        chips.chip(&[("gamma", style), ("!", style)]);
        chips.chip(&[("delta", style)]);
        chips.chip(&[("epsilon", style)]);
        chips.finish(&palette);
        let row = |y: u16| -> String {
            (0..12)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        };
        assert_eq!(row(0), "alpha  beta ");
        assert_eq!(row(1), "gamma!     …");
    }
}
