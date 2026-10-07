//! The tabs sidebar's Scheduled block (fork, sidebar v3): the tabs with a
//! scheduled reminder (`tab.set_reminder` every 5m … daily, the `◷ ◑ ☼`
//! row marks), under the Pinned block, with each interval and the time to
//! its next reminder (a countdown, or the daily time), or `fired` while a
//! reminder waits for the tab to be looked at. Hidden while no reminder is
//! set or when `ui.sidebar_scheduled_agents = false`. No new scheduling:
//! the reminders are the client's (`idle_reminders.rs`).
//!
//! The entries come from `SidebarModel::scheduled` (rebuilt on data change
//! only). The reminder clocks move without a snapshot change, so they are
//! read live per frame: one pass over the client's reminder map for the
//! active endpoint, matched against the shown entries (at most the expanded
//! cap) into a stack array; no allocation beyond one tab id clone per entry
//! for the hit map. A countdown records when its text next changes
//! (`hits.sidebar_clock_deadline`), so the client repaints at most once a
//! minute and only while a countdown shows.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::render::{display_width, put_text, put_truncated, ShellRenderState};
use super::sidebar_model::{
    format_countdown, interval_rank, next_countdown_tick, ScheduledView, SidebarHover,
    SidebarModel, StackStr, KIND_DAILY, KIND_HOURS, KIND_MINUTES,
};
use super::tab_sidebar_active::{
    hover_band, line_plan, put_group_text, render_overflow_row, render_section_header,
    section_frame, section_rows, HeaderMarks, HeaderRight,
};
use super::*;
use crate::api::schema::TabRemindInterval;

/// The block title.
pub(super) const SCHEDULED_TITLE: &str = "Scheduled";
/// A reminder that fired and waits for its tab to be looked at.
pub(super) const FIRED: &str = "fired";
/// The most entry lines while expanded (also the live-clock array's size).
const EXPANDED_CAP: u16 = 12;
/// The interval column's width (`daily`).
const INTERVAL_CELLS: u16 = 5;
/// The when column's width (`in 5h59`).
const WHEN_CELLS: u16 = 7;

/// The most entry lines the block shows (folded past it into `+N more`) at
/// a sidebar content height.
pub(super) fn scheduled_cap(content_height: u16) -> u16 {
    if content_height >= 36 {
        3
    } else {
        2
    }
}

/// The rows the block takes (rule, header, entry lines, `+N more`); 0
/// while disabled or empty.
pub(super) fn scheduled_block_rows(model: &SidebarModel, view: ScheduledView, cap: u16) -> u16 {
    section_rows(model.scheduled.len(), view, cap, EXPANDED_CAP)
}

/// One entry's live reminder clock, read once per frame.
#[derive(Debug, Clone, Copy, Default)]
struct Clock {
    /// The reminder is in the client's map (its clock started).
    known: bool,
    lit: bool,
    next_fire: Option<std::time::Instant>,
}

/// What an entry's when column shows.
enum When {
    Nothing,
    Fired,
    Countdown(StackStr<12>),
    Daily(StackStr<8>),
}

/// Draws the block into `rect` (already painted with the chrome background)
/// and registers its hits.
#[allow(clippy::too_many_arguments)] // the block's inputs; a struct would only rename them
pub(super) fn render_scheduled_block(
    buffer: &mut Buffer,
    rect: Rect,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    view: ScheduledView,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    if rect.is_empty() || !view.enabled || model.scheduled.is_empty() {
        return;
    }
    let palette = &config.palette;
    let hover = state.sidebar_hover;
    let lit_color = super::idle_reminders::ClientReminderLit::Scheduled.color(palette);
    let Some((header, lines)) = section_frame(buffer, rect, view.folded, palette) else {
        return;
    };
    let right = if view.folded {
        // One marker per present interval kind, lit while a reminder of
        // that kind waits (one pass over the reminder map).
        let mut lit_kinds = 0u8;
        for ((endpoint_id, _), reminder) in state.scheduled_reminders.iter() {
            if reminder.lit && endpoint_id == state.active_endpoint_id {
                if let Some((kind, _)) = interval_rank(reminder.every()) {
                    lit_kinds |= 1 << kind;
                }
            }
        }
        let mut marks = HeaderMarks::default();
        for (kind, every) in [
            (KIND_MINUTES, TabRemindInterval::M5),
            (KIND_HOURS, TabRemindInterval::H1),
            (KIND_DAILY, TabRemindInterval::Daily),
        ] {
            if model.scheduled_kinds & (1 << kind) != 0 {
                let color = if lit_kinds & (1 << kind) != 0 {
                    lit_color
                } else {
                    palette.overlay0
                };
                marks.push(super::tab_sidebar::remind_marker(every), color);
            }
        }
        HeaderRight::Marks(marks)
    } else {
        HeaderRight::Count(model.scheduled.len())
    };
    render_section_header(
        buffer,
        header,
        SCHEDULED_TITLE,
        view.folded,
        right,
        matches!(hover, Some(SidebarHover::ScheduledHeader)),
        palette,
    );
    hits.sidebar_scheduled_header = header;
    if lines == 0 {
        return;
    }
    let (shown, overflow) = line_plan(model.scheduled.len(), view.expanded, lines);
    let visible = &model.scheduled[..shown
        .min(model.scheduled.len())
        .min(usize::from(EXPANDED_CAP))];

    // The live clocks of the shown entries: one pass over the map.
    let mut clocks = [Clock::default(); EXPANDED_CAP as usize];
    for ((endpoint_id, tab_id), reminder) in state.scheduled_reminders.iter() {
        if endpoint_id != state.active_endpoint_id {
            continue;
        }
        let Some(slot) = visible.iter().position(|index| {
            snapshot
                .tabs
                .get(*index as usize)
                .is_some_and(|tab| tab.tab_id == *tab_id)
        }) else {
            continue;
        };
        clocks[slot] = Clock {
            known: true,
            lit: reminder.lit,
            next_fire: reminder.next_fire(),
        };
    }

    let mut deadline = hits.sidebar_clock_deadline;
    let mut whens: [When; EXPANDED_CAP as usize] = std::array::from_fn(|_| When::Nothing);
    for (slot, &index) in visible.iter().enumerate() {
        let Some(tab) = snapshot.tabs.get(index as usize) else {
            continue;
        };
        let clock = clocks[slot];
        whens[slot] = if !clock.known {
            When::Nothing
        } else if clock.lit && !tab.focused {
            When::Fired
        } else if tab.remind_every == Some(TabRemindInterval::Daily) {
            let mut text = StackStr::new();
            let minutes = config.daily_reminder_minutes;
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60),
            );
            When::Daily(text)
        } else {
            match clock.next_fire {
                Some(next_fire) => match next_countdown_tick(next_fire, state.now) {
                    Some(tick) => {
                        deadline = Some(deadline.map_or(tick, |current| current.min(tick)));
                        When::Countdown(format_countdown(
                            next_fire.saturating_duration_since(state.now),
                        ))
                    }
                    // Due: the reminder's own tick repaints.
                    None => When::Nothing,
                },
                None => When::Nothing,
            }
        };
    }
    hits.sidebar_clock_deadline = deadline;
    let has_when = whens[..visible.len()]
        .iter()
        .any(|when| !matches!(when, When::Nothing));

    // Columns from the right: margin, when, gap, interval, gap.
    let width = rect.width;
    let mut right_edge = width.saturating_sub(1);
    let when_x = has_when.then(|| {
        right_edge = right_edge.saturating_sub(WHEN_CELLS);
        let x = right_edge;
        right_edge = right_edge.saturating_sub(1);
        x
    });
    right_edge = right_edge.saturating_sub(INTERVAL_CELLS);
    let interval_x = right_edge;
    let name_end = right_edge.saturating_sub(1);

    let first_y = header.bottom();
    for (slot, &index) in visible.iter().enumerate() {
        let (Some(tab), Some(facts)) = (snapshot.tabs.get(index as usize), model.tab(index)) else {
            continue;
        };
        let row = Rect::new(rect.x, first_y + slot as u16, width, 1);
        let hovered = matches!(hover, Some(SidebarHover::ScheduledEntry(id)) if *id == tab.tab_id);
        if hovered {
            hover_band(buffer, row, palette);
        }
        let every = tab.remind_every.unwrap_or(TabRemindInterval::Unknown);
        let fired = matches!(whens[slot], When::Fired);
        put_text(
            buffer,
            row.x + 3,
            row.y,
            1,
            super::tab_sidebar::remind_marker(every),
            Style::default().fg(if fired { lit_color } else { palette.overlay0 }),
        );
        let name_x = 5;
        let name_width = name_end.saturating_sub(name_x);
        let name_fg = super::tab_color::tab_label_fg(tab.color, palette).unwrap_or(palette.text);
        put_truncated(
            buffer,
            row.x + name_x,
            row.y,
            name_width,
            &tab.label,
            Style::default().fg(name_fg),
        );
        let used = display_width(&tab.label).min(name_width);
        put_group_text(
            buffer,
            row.x + name_x + used + 1,
            row.y,
            name_width.saturating_sub(used + 1),
            snapshot,
            facts,
            state.teams,
            palette,
        );
        let name = every.name();
        let name_cells = display_width(name).min(INTERVAL_CELLS);
        put_text(
            buffer,
            row.x + interval_x + (INTERVAL_CELLS - name_cells),
            row.y,
            name_cells,
            name,
            Style::default().fg(palette.overlay0),
        );
        if let Some(x) = when_x {
            let (text, style) = match &whens[slot] {
                When::Nothing => ("", Style::default()),
                When::Fired => (
                    FIRED,
                    Style::default().fg(lit_color).add_modifier(Modifier::BOLD),
                ),
                When::Countdown(text) => (text.as_str(), Style::default().fg(palette.subtext0)),
                When::Daily(text) => (text.as_str(), Style::default().fg(palette.subtext0)),
            };
            let cells = display_width(text).min(WHEN_CELLS);
            put_text(
                buffer,
                row.x + x + (WHEN_CELLS - cells),
                row.y,
                cells,
                text,
                style,
            );
        }
        hits.sidebar_scheduled_rows.push((row, tab.tab_id.clone()));
    }
    if let Some(hidden) = overflow {
        let row = Rect::new(rect.x, first_y + visible.len() as u16, width, 1);
        let hovered = matches!(hover, Some(SidebarHover::ScheduledMore));
        render_overflow_row(buffer, row, hidden, hovered, palette);
        hits.sidebar_scheduled_more = row;
    }
}

/// The soonest countdown among the shown Scheduled entries (the header's
/// detail strip): the entry's tab index and when it fires.
pub(super) fn soonest_countdown(
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    state: &ShellRenderState<'_>,
) -> Option<(u32, std::time::Instant)> {
    let mut soonest: Option<(u32, std::time::Instant)> = None;
    for ((endpoint_id, tab_id), reminder) in state.scheduled_reminders.iter() {
        if endpoint_id != state.active_endpoint_id {
            continue;
        }
        let Some(next_fire) = reminder.next_fire().filter(|fire| *fire > state.now) else {
            continue;
        };
        let Some(&index) = model.scheduled.iter().find(|index| {
            snapshot
                .tabs
                .get(**index as usize)
                .is_some_and(|tab| tab.tab_id == *tab_id)
        }) else {
            continue;
        };
        if soonest.is_none_or(|(_, current)| next_fire < current) {
            soonest = Some((index, next_fire));
        }
    }
    soonest
}

/// The input side of the block (`mouse.rs` calls in).
impl ClientShellState {
    /// A left press on the Scheduled block: the header folds it
    /// (persisted), the overflow row expands or shrinks it, an entry jumps
    /// to its tab. Never starts a drag. Returns whether the press was the
    /// block's.
    pub(super) fn sidebar_scheduled_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if super::contains(self.hits.sidebar_scheduled_header, point) {
            self.scheduled_agents_folded = Some(!self.scheduled_agents_folded.unwrap_or(false));
            outcome.repaint = true;
            self.persist_chrome_preferences(outcome);
            return true;
        }
        if super::contains(self.hits.sidebar_scheduled_more, point) {
            self.scheduled_expanded = !self.scheduled_expanded;
            outcome.repaint = true;
            return true;
        }
        let Some(tab_id) = self
            .hits
            .sidebar_scheduled_rows
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, tab_id)| tab_id.clone())
        else {
            return false;
        };
        self.jump_to_sidebar_tab(tab_id, outcome);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_step_down_with_height() {
        assert_eq!(scheduled_cap(45), 3);
        assert_eq!(scheduled_cap(30), 2);
        assert_eq!(scheduled_cap(12), 2);
    }
}
