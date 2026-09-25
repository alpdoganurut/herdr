//! Tab color tags (`tab.set_color`) on the client side: the theme color each
//! name maps to, and the swatch row that ends the tab context menu.
//!
//! The swatch row is the tab menu's last item (`ClientContextMenuAction::Color`).
//! It counts as one row for Up/Down; while it is highlighted Left/Right (and
//! h/l) move a swatch cursor kept in the menu target (`ClientTabMenuColor`),
//! and Enter or a click on a swatch sends `tab.set_color` and closes the menu.

use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::style::Color;

use super::*;
use crate::api::schema::TabColor;

/// Picker glyphs.
const NONE_GLYPH: &str = "\u{2205}"; // ∅
const SWATCH_GLYPH: &str = "\u{25A0}"; // ■
/// Every swatch is three cells wide (` ■ ` or `[■]`), so neighbouring glyphs
/// sit two cells apart and each hit rectangle is centred on its glyph.
pub(super) const SWATCH_WIDTH: u16 = 3;

/// The picker's choices in order: none first, then `TabColor::OFFERED`.
pub(super) fn picker_choices() -> impl Iterator<Item = Option<TabColor>> {
    std::iter::once(None).chain(TabColor::OFFERED.into_iter().map(Some))
}

/// The color a tab color is drawn in. Fixed true colors rather than theme
/// slots: 16-color themes (including `terminal`, which defers to the host
/// palette) remap the ANSI slots, so "yellow" can render teal and "blue"
/// can sit next to the gray of an unfocused label. Hues are spread so that
/// each reads apart from the others and from default text on dark and light
/// backgrounds. `Unknown` (a newer server's color) draws as no color. The
/// palette argument stays so a theme-aware mapping can return later.
pub(super) fn tab_color_fg(color: TabColor, _palette: &Palette) -> Option<Color> {
    let (r, g, b) = match color {
        TabColor::Red => (0xF2, 0x4B, 0x4B),
        TabColor::Orange => (0xF2, 0x8C, 0x28),
        TabColor::Yellow => (0xF2, 0xC9, 0x4C),
        TabColor::Green => (0x3E, 0xCF, 0x6E),
        TabColor::Cyan => (0x2C, 0xC5, 0xD6),
        TabColor::Blue => (0x4A, 0x7D, 0xFF),
        TabColor::Purple => (0xB0, 0x6C, 0xF5),
        TabColor::Unknown => return None,
    };
    Some(Color::Rgb(r, g, b))
}

/// The label foreground for a tab: its color when it has a known one.
pub(super) fn tab_label_fg(color: Option<TabColor>, palette: &Palette) -> Option<Color> {
    color.and_then(|color| tab_color_fg(color, palette))
}

/// Width of the swatch row: three cells per choice.
pub(super) fn swatch_row_width() -> u16 {
    picker_choices().count() as u16 * SWATCH_WIDTH
}

/// The swatch row state for a tab menu opened on a tab with `color`: the
/// cursor starts on the current color, or on none when it is not offered.
pub(super) fn tab_menu_color(color: Option<TabColor>) -> ClientTabMenuColor {
    let current = color.filter(|color| *color != TabColor::Unknown);
    ClientTabMenuColor {
        current,
        cursor: current_swatch(current),
    }
}

fn current_swatch(current: Option<TabColor>) -> usize {
    picker_choices()
        .position(|choice| choice == current)
        .unwrap_or(0)
}

/// Draw the swatch row into `inner` (one menu row) and return one hit
/// rectangle per swatch, indexed like `picker_choices`. `highlighted` is the
/// swatch cursor while the row is the menu's highlighted row.
pub(super) fn render_swatches(
    buffer: &mut Buffer,
    inner: Rect,
    current: Option<TabColor>,
    highlighted: Option<usize>,
    palette: &Palette,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    for (index, choice) in picker_choices().enumerate() {
        let x = inner.x.saturating_add(index as u16 * SWATCH_WIDTH);
        if x.saturating_add(SWATCH_WIDTH) > inner.right() || inner.height == 0 {
            break;
        }
        let rect = Rect::new(x, inner.y, SWATCH_WIDTH, 1);
        let is_highlighted = highlighted == Some(index);
        let base = if is_highlighted {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.panel_bg)
        };
        let glyph_fg = match choice {
            None => palette.overlay0,
            Some(color) => tab_color_fg(color, palette).unwrap_or(palette.overlay0),
        };
        let (open, close) = if choice == current {
            ("[", "]")
        } else {
            (" ", " ")
        };
        let glyph = if choice.is_none() {
            NONE_GLYPH
        } else {
            SWATCH_GLYPH
        };
        super::render::put_text(buffer, rect.x, rect.y, 1, open, base);
        super::render::put_text(buffer, rect.x + 1, rect.y, 1, glyph, base.fg(glyph_fg));
        super::render::put_text(buffer, rect.x + 2, rect.y, 1, close, base);
        hits.push((rect, index));
    }
    hits
}

impl ClientShellState {
    /// The swatch row's item index in the open tab menu, with its state.
    fn tab_menu_color_row(&self) -> Option<(usize, usize, ClientTabMenuColor)> {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_ref() else {
            return None;
        };
        let ClientContextMenuTarget::Tab { color, .. } = &menu.target else {
            return None;
        };
        let row = menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::Color)?;
        Some((row, menu.highlighted, *color))
    }

    fn set_tab_menu_color(&mut self, highlighted: usize, cursor: usize) {
        if let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { color, .. },
            highlighted: menu_highlighted,
            ..
        })) = self.overlay.as_mut()
        {
            *menu_highlighted = highlighted;
            color.cursor = cursor;
        }
    }

    /// Tab menu keys for the swatch row: Up/Down move between rows (entering
    /// the swatch row puts the cursor on the current color), Left/Right and
    /// h/l move the swatch cursor while the row is highlighted. Returns
    /// whether the key was used; everything else goes to the menu.
    pub(super) fn route_tab_color_menu_key(&mut self, code: KeyCode) -> bool {
        let Some((row, highlighted, color)) = self.tab_menu_color_row() else {
            return false;
        };
        let last = picker_choices().count().saturating_sub(1);
        match code {
            KeyCode::Up | KeyCode::Down => {
                self.move_context_menu_selection(if code == KeyCode::Up { -1 } else { 1 });
                let entered = self
                    .tab_menu_color_row()
                    .is_some_and(|(_, now, _)| now == row && highlighted != row);
                if entered {
                    self.set_tab_menu_color(row, current_swatch(color.current));
                }
                true
            }
            KeyCode::Left | KeyCode::Char('h') if highlighted == row => {
                self.set_tab_menu_color(row, color.cursor.saturating_sub(1));
                true
            }
            KeyCode::Right | KeyCode::Char('l') if highlighted == row => {
                self.set_tab_menu_color(row, (color.cursor + 1).min(last));
                true
            }
            _ => false,
        }
    }

    /// Mouse over the swatch row: hovering a swatch highlights the row and
    /// moves the cursor there, a left click picks it. Returns whether the
    /// event hit a swatch.
    pub(super) fn route_tab_color_swatch_mouse(
        &mut self,
        point: (u16, u16),
        kind: MouseEventKind,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(swatch) = self
            .hits
            .context_menu_swatches
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, swatch)| *swatch)
        else {
            return false;
        };
        let Some((row, _, _)) = self.tab_menu_color_row() else {
            return false;
        };
        match kind {
            MouseEventKind::Moved => {
                self.set_tab_menu_color(row, swatch);
                outcome.repaint = true;
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_tab_menu_color(row, swatch);
                self.activate_context_menu_item(row, outcome);
                true
            }
            _ => false,
        }
    }

    /// `tab.set_color` with the swatch at `cursor` (none clears).
    pub(super) fn pick_tab_color_swatch(
        &mut self,
        tab_id: String,
        cursor: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(color) = picker_choices().nth(cursor) else {
            return;
        };
        self.push_endpoint_method(
            crate::api::schema::Method::TabSetColor(crate::api::schema::TabSetColorParams {
                tab_id,
                color,
            }),
            outcome,
        );
    }
}
