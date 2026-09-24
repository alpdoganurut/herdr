//! Tab color tags (`tab.set_color`) on the client side: the theme color each
//! name maps to, and the swatch picker the tab menu's "Color" item opens.
//!
//! The picker is a `ClientContextMenuOverlay` with a `TabColor` target, so it
//! reuses the context menu's overlay slot, hit rectangles
//! (`hits.context_menu_rows`), mouse handling and Enter/Esc keys; only its
//! drawing (one horizontal row of swatches) and Left/Right navigation differ.

use ratatui::style::Color;

use super::*;
use crate::api::schema::TabColor;

/// Picker glyphs.
const NONE_GLYPH: &str = "\u{2205}"; // ∅
const SWATCH_GLYPH: &str = "\u{25A0}"; // ■
/// Every swatch is three cells wide (` ■ ` or `[■]`), so neighbouring glyphs
/// sit two cells apart and each hit rectangle is centred on its glyph.
pub(super) const SWATCH_WIDTH: u16 = 3;

/// The picker's choices in order: none first, then `TabColor::ALL`.
pub(super) fn picker_choices() -> impl Iterator<Item = Option<TabColor>> {
    std::iter::once(None).chain(TabColor::ALL.into_iter().map(Some))
}

/// The theme color a tab color is drawn in. The palette has no orange or
/// purple slot, so orange uses `peach` and purple uses `mauve`; cyan uses
/// `teal`. `Unknown` (a newer server's color) draws as no color.
pub(super) fn tab_color_fg(color: TabColor, palette: &Palette) -> Option<Color> {
    match color {
        TabColor::Red => Some(palette.red),
        TabColor::Orange => Some(palette.peach),
        TabColor::Yellow => Some(palette.yellow),
        TabColor::Green => Some(palette.green),
        TabColor::Cyan => Some(palette.teal),
        TabColor::Blue => Some(palette.blue),
        TabColor::Purple => Some(palette.mauve),
        TabColor::Unknown => None,
    }
}

/// The label foreground for a tab: its color when it has a known one.
pub(super) fn tab_label_fg(color: Option<TabColor>, palette: &Palette) -> Option<Color> {
    color.and_then(|color| tab_color_fg(color, palette))
}

/// Outer size of the picker box: one swatch row inside a border.
pub(super) fn picker_size() -> (u16, u16) {
    let swatches = picker_choices().count() as u16;
    (swatches * SWATCH_WIDTH + 2, 3)
}

/// Draw the swatch row into the picker's inner rectangle and return one hit
/// rectangle per swatch, indexed like `picker_choices`.
pub(super) fn render_swatches(
    buffer: &mut Buffer,
    inner: Rect,
    current: Option<TabColor>,
    highlighted: usize,
    palette: &Palette,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    for (index, choice) in picker_choices().enumerate() {
        let x = inner.x.saturating_add(index as u16 * SWATCH_WIDTH);
        if x.saturating_add(SWATCH_WIDTH) > inner.right() || inner.height == 0 {
            break;
        }
        let rect = Rect::new(x, inner.y, SWATCH_WIDTH, 1);
        let is_highlighted = index == highlighted;
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
    /// Replace the tab menu with the color picker for `tab_id`, at the same
    /// anchor, with the tab's current color highlighted.
    pub(super) fn open_tab_color_picker(&mut self, tab_id: String, x: u16, y: u16) {
        let Some(tab) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id))
        else {
            return;
        };
        let current = tab.color.filter(|color| *color != TabColor::Unknown);
        let highlighted = picker_choices()
            .position(|choice| choice == current)
            .unwrap_or(0);
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::TabColor { tab_id, current },
            x,
            y,
            highlighted,
        }));
    }

    /// Picker keys beyond the context menu's Up/Down/Enter/Esc: Left/Right
    /// and h/l move the highlight along the row. Returns whether the key was
    /// used.
    pub(super) fn route_tab_color_picker_key(&mut self, code: KeyCode) -> bool {
        let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::TabColor { .. },
            ..
        })) = self.overlay.as_ref()
        else {
            return false;
        };
        match code {
            KeyCode::Left | KeyCode::Char('h') => self.move_context_menu_selection(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_context_menu_selection(1),
            _ => return false,
        }
        true
    }

    /// A picked swatch: `tab.set_color` for the tab the picker opened on.
    pub(super) fn activate_tab_color_choice(
        &mut self,
        tab_id: String,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        if let ClientContextMenuAction::SetTabColor(color) = action {
            self.push_endpoint_method(
                crate::api::schema::Method::TabSetColor(crate::api::schema::TabSetColorParams {
                    tab_id,
                    color,
                }),
                outcome,
            );
        }
    }
}
