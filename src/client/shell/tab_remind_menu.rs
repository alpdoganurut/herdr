//! The tab menu's scheduled reminder selector: two rows directly before the
//! color swatch row (`ClientContextMenuAction::RemindTop` / `RemindBottom`),
//!
//! ```text
//! remind  5m  10m  30m
//!         1h  6h   daily
//! ```
//!
//! one selector across both rows, the options in columns. The current
//! interval is bracketed. Left/Right (and h/l) move within a row, Up/Down move
//! between the two rows at the same column and past them to the neighbouring
//! menu items; entering the selector puts the cursor on the current interval
//! when it is on the entered row, else on that row's first column. Enter or a
//! click picks the interval under the cursor with `tab.set_reminder`; picking
//! the current one again turns the reminder off. Picking never focuses the
//! tab, since that would mark its agent seen.

use crossterm::event::{MouseButton, MouseEventKind};

use super::*;
use crate::api::schema::{TabRemindEvery, TabRemindInterval};

/// The label drawn before the first row's options.
const ROW_LABEL: &str = "remind";
/// Options per row.
const COLUMNS: usize = 3;

/// A column's width: its widest option plus a bracket (or space) each side.
fn column_width(column: usize) -> u16 {
    [column, column + COLUMNS]
        .into_iter()
        .map(|index| TabRemindInterval::ALL[index].name().len())
        .max()
        .unwrap_or(0) as u16
        + 2
}

fn column_x(column: usize) -> u16 {
    ROW_LABEL.len() as u16 + 1 + (0..column).map(column_width).sum::<u16>()
}

/// Width of a selector row: the label, a gap, the columns.
pub(super) fn remind_row_width() -> u16 {
    column_x(COLUMNS)
}

/// The selector state for a tab menu opened on a tab with `every`.
pub(super) fn tab_menu_remind(every: Option<TabRemindInterval>) -> ClientTabMenuRemind {
    let current = every.filter(|every| *every != TabRemindInterval::Unknown);
    ClientTabMenuRemind {
        current,
        cursor: current_option(current).unwrap_or(0),
    }
}

fn current_option(current: Option<TabRemindInterval>) -> Option<usize> {
    current.and_then(|every| {
        TabRemindInterval::ALL
            .iter()
            .position(|option| *option == every)
    })
}

/// The cursor on `row` (0 top, 1 bottom) when entering it: the current
/// interval if it is on that row, else the row's first column.
fn entry_cursor(row: usize, current: Option<TabRemindInterval>) -> usize {
    current_option(current)
        .filter(|option| option / COLUMNS == row)
        .unwrap_or(row * COLUMNS)
}

/// What picking the option at `cursor` sets: that interval, or off when it
/// is already the current one.
pub(super) fn picked_setting(remind: ClientTabMenuRemind) -> Option<TabRemindEvery> {
    let every = *TabRemindInterval::ALL.get(remind.cursor)?;
    Some(if remind.current == Some(every) {
        TabRemindEvery::Off
    } else {
        TabRemindEvery::from_interval(Some(every))
    })
}

/// Draw selector row `row` (0 top, 1 bottom) into `rect` and return one hit
/// rectangle per option, indexed like `TabRemindInterval::ALL`. `highlighted`
/// is the cursor while this row is the menu's highlighted row.
pub(super) fn render_remind_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: usize,
    remind: ClientTabMenuRemind,
    highlighted: Option<usize>,
    palette: &Palette,
) -> Vec<(Rect, usize)> {
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_style(rect, base);
    if row == 0 {
        super::render::put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            ROW_LABEL,
            base.fg(palette.overlay1),
        );
    }
    let mut hits = Vec::new();
    for column in 0..COLUMNS {
        let index = row * COLUMNS + column;
        let every = TabRemindInterval::ALL[index];
        let name_width = every.name().len() as u16 + 2;
        let x = rect.x.saturating_add(column_x(column));
        if x.saturating_add(name_width) > rect.right() {
            break;
        }
        let hit = Rect::new(x, rect.y, name_width, 1);
        let current = remind.current == Some(every);
        let style = if highlighted == Some(index) {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else if current {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        let text = if current {
            format!("[{}]", every.name())
        } else {
            format!(" {} ", every.name())
        };
        super::render::put_text(buffer, hit.x, hit.y, hit.width, &text, style);
        hits.push((hit, index));
    }
    hits
}

impl ClientShellState {
    /// The selector's first row index in the open tab menu, with the
    /// highlight and the selector state.
    fn tab_menu_remind_rows(&self) -> Option<(usize, usize, ClientTabMenuRemind)> {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_ref() else {
            return None;
        };
        let ClientContextMenuTarget::Tab { remind, .. } = &menu.target else {
            return None;
        };
        let top = menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::RemindTop)?;
        Some((top, menu.highlighted, *remind))
    }

    fn set_tab_menu_remind(&mut self, highlighted: usize, cursor: usize) {
        if let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { remind, .. },
            highlighted: menu_highlighted,
            ..
        })) = self.overlay.as_mut()
        {
            *menu_highlighted = highlighted;
            remind.cursor = cursor;
        }
    }

    /// Tab menu keys for the selector (see the module docs). Moves that
    /// leave the selector go through the swatch row's router, which also
    /// seats the swatch cursor. Returns whether the key was used.
    pub(super) fn route_tab_remind_menu_key(&mut self, code: KeyCode) -> bool {
        let Some((top, highlighted, remind)) = self.tab_menu_remind_rows() else {
            return false;
        };
        let bottom = top + 1;
        let in_selector = highlighted == top || highlighted == bottom;
        let column = remind.cursor % COLUMNS;
        match code {
            KeyCode::Down if highlighted == top => {
                self.set_tab_menu_remind(bottom, COLUMNS + column);
                true
            }
            KeyCode::Up if highlighted == bottom => {
                self.set_tab_menu_remind(top, column);
                true
            }
            KeyCode::Up | KeyCode::Down => {
                if !self.route_tab_color_menu_key(code) {
                    self.move_context_menu_selection(if code == KeyCode::Up { -1 } else { 1 });
                }
                if let Some((_, now, _)) = self.tab_menu_remind_rows() {
                    if !in_selector && (now == top || now == bottom) {
                        let row = now - top;
                        self.set_tab_menu_remind(now, entry_cursor(row, remind.current));
                    }
                }
                true
            }
            KeyCode::Left | KeyCode::Char('h') if in_selector => {
                let row_start = remind.cursor - column;
                self.set_tab_menu_remind(highlighted, row_start + column.saturating_sub(1));
                true
            }
            KeyCode::Right | KeyCode::Char('l') if in_selector => {
                let row_start = remind.cursor - column;
                self.set_tab_menu_remind(highlighted, row_start + (column + 1).min(COLUMNS - 1));
                true
            }
            _ => false,
        }
    }

    /// Mouse over the selector: hovering an option highlights its row and
    /// moves the cursor there, a left click picks it. Returns whether the
    /// event hit an option.
    pub(super) fn route_tab_remind_option_mouse(
        &mut self,
        point: (u16, u16),
        kind: MouseEventKind,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(option) = self
            .hits
            .context_menu_remind_options
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, option)| *option)
        else {
            return false;
        };
        let Some((top, _, _)) = self.tab_menu_remind_rows() else {
            return false;
        };
        let row = top + option / COLUMNS;
        match kind {
            MouseEventKind::Moved => {
                self.set_tab_menu_remind(row, option);
                outcome.repaint = true;
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_tab_menu_remind(row, option);
                self.activate_context_menu_item(row, outcome);
                true
            }
            _ => false,
        }
    }

    /// `tab.set_reminder` for the option under the selector's cursor.
    pub(super) fn pick_tab_remind_option(
        &mut self,
        tab_id: String,
        remind: ClientTabMenuRemind,
        outcome: &mut ClientShellInput,
    ) {
        let Some(every) = picked_setting(remind) else {
            return;
        };
        self.push_endpoint_method(
            crate::api::schema::Method::TabSetReminder(crate::api::schema::TabSetReminderParams {
                tab_id,
                important: None,
                every: Some(every),
            }),
            outcome,
        );
    }
}
