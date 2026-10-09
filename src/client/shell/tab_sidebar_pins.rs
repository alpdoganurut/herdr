//! The tabs sidebar's Pinned block (fork, sidebar v3): the tabs pinned from
//! the tab menu (`tab.set_pinned`, server-owned, pushed as
//! `endpoint.tab-pins.v1`), under the Active block. A pinned tab also stays
//! in its group. Hidden while nothing is pinned or when
//! `ui.sidebar_pinned_agents = false`.
//!
//! The entries come from `SidebarModel::pins` (snapshot order, rebuilt on
//! data change only). A frame draws the rule, the header and at most
//! `rect.height - 2` entry lines in the Active block's entry format (status
//! icon, voice mark, label, team or group, subagents, time in state), with
//! no allocation beyond one tab id clone per entry for the hit map.

use ratatui::{buffer::Buffer, layout::Rect};

use super::render::ShellRenderState;
use super::sidebar_model::{
    PinsView, SidebarHover, SidebarModel, CLASS_BLOCKED, CLASS_DONE, CLASS_SUBAGENTS, CLASS_VOICE,
    CLASS_WORKING,
};
use super::tab_sidebar_active::{
    class_mark, line_plan, render_entry, render_overflow_row, render_section_header, section_frame,
    section_rows, EntryColumns, EntryLine, HeaderMarks, HeaderRight,
};
use super::*;

/// The block title.
pub(super) const PINS_TITLE: &str = "Pinned";
/// The most entry lines while expanded.
const EXPANDED_CAP: u16 = 12;

/// The most entry lines the block shows (folded past it into `+N more`) at
/// a sidebar content height.
pub(super) fn pins_cap(content_height: u16) -> u16 {
    if content_height >= 36 {
        4
    } else if content_height >= 28 {
        3
    } else {
        2
    }
}

/// The rows the block takes (rule, header, entry lines, `+N more`); 0
/// while disabled or empty.
pub(super) fn pins_block_rows(model: &SidebarModel, view: PinsView, cap: u16) -> u16 {
    section_rows(model.pins.len(), view, cap, EXPANDED_CAP)
}

/// Draws the block into `rect` (already painted with the chrome background)
/// and registers its hits.
#[allow(clippy::too_many_arguments)] // the block's inputs; a struct would only rename them
pub(super) fn render_pins_block(
    buffer: &mut Buffer,
    rect: Rect,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    view: PinsView,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    if rect.is_empty() || !view.enabled || model.pins.is_empty() {
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
            if model.pins_classes & (1 << class) != 0 {
                let (glyph, color) = class_mark(class, config);
                marks.push(glyph, color);
            }
        }
        HeaderRight::Marks(marks, model.pins.len())
    } else {
        HeaderRight::Count(model.pins.len())
    };
    render_section_header(
        buffer,
        header,
        PINS_TITLE,
        view.folded,
        right,
        matches!(hover, Some(SidebarHover::PinsHeader)),
        palette,
    );
    hits.sidebar_pins_header = header;
    if lines == 0 {
        return;
    }
    let (shown, overflow) = line_plan(model.pins.len(), view.expanded, lines);
    let visible = &model.pins[..shown.min(model.pins.len())];
    let columns =
        EntryColumns::for_tabs(visible.iter().copied(), snapshot, model, rect.width, true);
    let mut deadline = hits.sidebar_clock_deadline;
    let first_y = header.bottom();
    for (offset, &tab_index) in visible.iter().enumerate() {
        let Some(tab) = snapshot.tabs.get(tab_index as usize) else {
            continue;
        };
        let row = Rect::new(rect.x, first_y + offset as u16, rect.width, 1);
        let hovered = matches!(hover, Some(SidebarHover::PinsEntry(id)) if *id == tab.tab_id);
        let line = EntryLine {
            columns,
            hovered,
            now: state.now,
            teams: state.teams,
        };
        if let Some(tick) = render_entry(buffer, row, snapshot, model, tab_index, line, config) {
            deadline = Some(deadline.map_or(tick, |current| current.min(tick)));
        }
        hits.sidebar_pins_rows.push((row, tab.tab_id.clone()));
    }
    hits.sidebar_clock_deadline = deadline;
    if let Some(hidden) = overflow {
        let row = Rect::new(rect.x, first_y + visible.len() as u16, rect.width, 1);
        let hovered = matches!(hover, Some(SidebarHover::PinsMore));
        render_overflow_row(buffer, row, hidden, hovered, palette);
        hits.sidebar_pins_more = row;
    }
}

/// The input side of the block (`mouse.rs` calls in).
impl ClientShellState {
    /// A left press on the Pinned block: the header folds it (persisted),
    /// the overflow row expands or shrinks it, an entry jumps to its tab.
    /// Never starts a drag. Returns whether the press was the block's.
    pub(super) fn sidebar_pins_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if super::contains(self.hits.sidebar_pins_header, point) {
            self.pinned_agents_folded = Some(!self.pinned_agents_folded.unwrap_or(false));
            outcome.repaint = true;
            self.persist_chrome_preferences(outcome);
            return true;
        }
        if super::contains(self.hits.sidebar_pins_more, point) {
            self.pins_expanded = !self.pins_expanded;
            outcome.repaint = true;
            return true;
        }
        let Some(tab_id) = self
            .hits
            .sidebar_pins_rows
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
        assert_eq!(pins_cap(45), 4);
        assert_eq!(pins_cap(30), 3);
        assert_eq!(pins_cap(20), 2);
        assert_eq!(pins_cap(12), 2);
    }
}
