//! Suspended-pane presentation for the tabs layout.
//!
//! A parked agent leaves a bare shell behind. In the tabs layout that pane is
//! shown as a card instead of the shell (nothing of the old screen bleeds
//! through), and every key, text, paste, and mouse-report event aimed at the
//! pane is dropped: the only way back is the `toggle_agent_suspend` binding
//! or the tab menu, so a stray keystroke can never land in the parked shell.
//! Everything here is client presentation; the server state is untouched.

use ratatui::layout::Rect;

use super::*;
use crate::protocol::{color_to_u32, CellData, FrameData};

impl ClientShellState {
    /// Rebuild the suspended pane set from the active snapshot.
    pub(super) fn refresh_suspended_pane_ids(&mut self) {
        self.suspended_pane_ids.clear();
        if let Some(snapshot) = self.snapshot.as_deref() {
            self.suspended_pane_ids.extend(
                snapshot
                    .agents
                    .iter()
                    .filter(|agent| {
                        agent.agent_status == crate::api::schema::AgentStatus::Suspended
                    })
                    .map(|agent| agent.pane_id.clone()),
            );
        }
    }

    /// Whether the endpoint reports `pane_id` as a suspended agent pane.
    pub(super) fn pane_suspended(&self, pane_id: &str) -> bool {
        self.suspended_pane_ids.contains(pane_id)
    }

    /// Pane input is locked while the pane is suspended in the tabs layout.
    pub(super) fn suspended_pane_locked(&self, pane_id: &str) -> bool {
        self.config.sidebar_layout == crate::config::SidebarLayoutConfig::Tabs
            && self.pane_suspended(pane_id)
    }

    /// The tabs layout keeps one pane per tab: no splits, swaps, zoom, or resize.
    pub(super) fn pane_topology_locked(&self) -> bool {
        self.config.sidebar_layout == crate::config::SidebarLayoutConfig::Tabs
    }

    /// Replace every suspended pane's screen with its card. Runs after the pane
    /// surface is blitted so the card covers the shell rather than the chrome.
    /// Returns the painted areas so graphics can be occluded under them.
    pub(super) fn paint_suspended_panes(
        &self,
        frame: &mut FrameData,
        snapshot: &ClientShellSnapshot,
    ) -> Vec<Rect> {
        let mut painted = Vec::new();
        if self.config.sidebar_layout != crate::config::SidebarLayoutConfig::Tabs
            || self.suspended_pane_ids.is_empty()
        {
            return painted;
        }
        for hit in &self.hits.panes {
            if hit.popup || !self.pane_suspended(&hit.pane_id) {
                continue;
            }
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == hit.pane_id);
            let pane = snapshot
                .panes
                .iter()
                .find(|pane| pane.pane_id == hit.pane_id);
            let title = agent
                .and_then(|agent| {
                    agent
                        .name
                        .as_deref()
                        .or(agent.display_agent.as_deref())
                        .or(agent.agent.as_deref())
                })
                .unwrap_or("agent");
            let cwd = pane.and_then(|pane| pane.cwd.as_deref()).unwrap_or("");
            let key = self.config.keybinds.keybinds.toggle_agent_suspend.label();
            let hint = match key.as_deref() {
                Some(key) => format!("{key} to activate"),
                None => "bind keys.toggle_agent_suspend to activate".to_string(),
            };
            let palette = &self.config.palette;
            let lines: [(String, ratatui::style::Color); 4] = [
                (
                    format!(
                        "{} suspended",
                        status_icon(
                            crate::api::schema::AgentStatus::Suspended,
                            self.config.status_indicators
                        )
                    ),
                    palette.text,
                ),
                (title.to_string(), palette.subtext0),
                (cwd.to_string(), palette.overlay0),
                (hint, palette.accent),
            ];
            paint_card(frame, hit.inner_rect, &lines, palette);
            painted.push(hit.inner_rect);
            if let Some(cursor) = frame.cursor.as_ref() {
                let inside = cursor.x >= hit.inner_rect.x
                    && cursor.x < hit.inner_rect.right()
                    && cursor.y >= hit.inner_rect.y
                    && cursor.y < hit.inner_rect.bottom();
                if inside {
                    frame.cursor = None;
                }
            }
        }
        painted
    }
}

fn paint_card(
    frame: &mut FrameData,
    area: Rect,
    lines: &[(String, ratatui::style::Color)],
    palette: &Palette,
) {
    if area.is_empty() {
        return;
    }
    let bg = color_to_u32(palette.sidebar_bg);
    let blank = |bg: u32| CellData {
        symbol: " ".into(),
        fg: bg,
        bg,
        modifier: 0,
        skip: false,
        hyperlink: None,
    };
    for y in area.y..area.bottom().min(frame.height) {
        for x in area.x..area.right().min(frame.width) {
            let index = usize::from(y) * usize::from(frame.width) + usize::from(x);
            if let Some(cell) = frame.cells.get_mut(index) {
                *cell = blank(bg);
            }
        }
    }
    // Clamp to the frame like the fill loop: a pane rect past the frame edge must
    // not spill into the next row.
    let area = Rect::new(
        area.x,
        area.y,
        area.width.min(frame.width.saturating_sub(area.x)),
        area.height.min(frame.height.saturating_sub(area.y)),
    );
    if area.is_empty() {
        return;
    }
    let visible = lines
        .iter()
        .filter(|(text, _)| !text.is_empty())
        .collect::<Vec<_>>();
    let block_height = visible.len().min(usize::from(area.height)) as u16;
    let top = area.y + area.height.saturating_sub(block_height) / 2;
    for (row, (text, color)) in visible.iter().take(usize::from(block_height)).enumerate() {
        let text = crate::ui::truncate_end(text, usize::from(area.width.saturating_sub(2)));
        let width = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
        let left = area.x + area.width.saturating_sub(width) / 2;
        let y = top + row as u16;
        let fg = color_to_u32(*color);
        let mut x = left;
        for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(text.as_str(), true) {
            let cells = unicode_width::UnicodeWidthStr::width(grapheme).max(1) as u16;
            if x + cells > area.right() {
                break;
            }
            let index = usize::from(y) * usize::from(frame.width) + usize::from(x);
            if let Some(cell) = frame.cells.get_mut(index) {
                *cell = CellData {
                    symbol: grapheme.to_string(),
                    fg,
                    bg,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                };
            }
            for extra in 1..cells {
                let index = usize::from(y) * usize::from(frame.width) + usize::from(x + extra);
                if let Some(cell) = frame.cells.get_mut(index) {
                    *cell = CellData {
                        symbol: String::new(),
                        fg,
                        bg,
                        modifier: 0,
                        skip: false,
                        hyperlink: None,
                    };
                }
            }
            x += cells;
        }
    }
}
