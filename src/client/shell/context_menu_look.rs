//! Fork (menu design): the visual language every context menu shares.
//!
//! ```text
//! ╭──────────────────────────────╮
//! │ + New tab                ⌥t  │   one glyph per item, the key hint dim
//! │ ✕ Close                ^b X  │   destructive: red (red cursor bar)
//! │─ Agent ──────────────────────│   a section rule with its title
//! │ ‖ Suspend agent          ⌥s  │
//! │─ Alerts ─────────────────────│
//! │ ★ Important              on  │   toggles keep one label: ○/● (☆/★)
//! │ ○ Pin                        │
//! ╰──────────────────────────────╯
//! ```
//!
//! Each [`ClientContextMenuItem`] carries a [`MenuLook`]: its glyph, its
//! section and its kind. [`menu_rows`] lays the items out with one rule row
//! before every section change; rule rows are drawn only, never selectable:
//! `highlighted` and the hit rows index the items, so keyboard moves and
//! clicks pass over them. Key hints come from the effective keybindings
//! ([`key_hint`]); an unbound action shows none. Colours are theme roles:
//! glyphs accent, labels text, section titles mauve, rules surface1, hints
//! and off toggles overlay0, on toggles green, the Important star and the
//! current reminder yellow, danger red.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::config::KeyCombo;

/// The narrowest menu, borders included.
const MIN_WIDTH: u16 = 24;
/// The widest menu, borders included (the sidebar is often ~30 columns):
/// a longer label and hint drop the hint.
const MAX_WIDTH: u16 = 34;
/// Cells before an item's label: a pad, the glyph, a gap.
pub(super) const LABEL_X: u16 = 3;
/// Cells after the hint (or the label) before the right border.
const RIGHT_PAD: u16 = 2;
/// The fewest cells between a label and its hint.
const HINT_GAP: u16 = 2;
/// A selector row's label cells (`remind  ` / `colour  `).
pub(super) const SELECTOR_LABEL_CELLS: u16 = 8;
/// The swatch row's label.
const COLOUR_LABEL: &str = "colour";
/// The hint of a toggle that is on.
const ON_HINT: &str = "on";

/// Where an item sits; a change of section draws a rule row first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum MenuSection {
    /// The menu's first block, no rule.
    #[default]
    Lead,
    /// A rule with a title (`─ Agent ───`).
    Titled(&'static str),
    /// The destructive block: a rule titled `Danger` in red.
    Danger,
    /// The last block behind an untitled rule (Close pane).
    Last,
}

/// How an item draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum MenuKind {
    #[default]
    Action,
    /// Destructive: red, with a red cursor bar.
    Danger,
    /// A toggle with a stable label: `○` off, a green `●` on.
    Toggle(bool),
    /// The Important toggle: `☆` off, a yellow `★` on.
    Star(bool),
    /// Not available now (the coordinator's): dim; a click says why.
    Disabled,
    /// The reminder selector and the swatch row (their own cursors).
    Selector,
}

/// An item's glyph, section and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct MenuLook {
    pub(super) glyph: &'static str,
    pub(super) section: MenuSection,
    pub(super) kind: MenuKind,
}

impl MenuLook {
    pub(super) const fn new(glyph: &'static str, section: MenuSection, kind: MenuKind) -> Self {
        Self {
            glyph,
            section,
            kind,
        }
    }
}

/// One drawn row of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuRow {
    /// A rule row before the items of this section (never selectable).
    Rule(MenuSection),
    /// The item at this index of `items()`.
    Item(usize),
}

/// The rows of `items`: a rule row before each section change (none before
/// a leading `Lead` block).
pub(super) fn menu_rows(items: &[ClientContextMenuItem]) -> Vec<MenuRow> {
    let mut rows = Vec::with_capacity(items.len() + 4);
    let mut previous: Option<MenuSection> = None;
    for (index, item) in items.iter().enumerate() {
        let section = item.look.section;
        let changed = match previous {
            None => section != MenuSection::Lead,
            Some(previous) => previous != section,
        };
        if changed {
            rows.push(MenuRow::Rule(section));
        }
        previous = Some(section);
        rows.push(MenuRow::Item(index));
    }
    rows
}

/// The keybinding an item's action has in this menu, if any.
fn item_binding<'a>(
    target: &ClientContextMenuTarget,
    action: ClientContextMenuAction,
    keys: &'a crate::config::Keybinds,
) -> Option<&'a crate::config::ActionKeybinds> {
    use ClientContextMenuAction as Action;
    use ClientContextMenuTarget as Target;
    Some(match (action, target) {
        (Action::NewTab, _) => &keys.new_tab,
        (Action::Rename, Target::Tab { .. }) => &keys.rename_tab,
        (Action::Rename, Target::Workspace { .. }) => &keys.rename_workspace,
        (Action::Close, Target::Tab { .. }) => &keys.close_tab,
        (Action::Close, Target::Workspace { .. }) => &keys.close_workspace,
        (Action::SuspendAgent | Action::ActivateAgent, _) => &keys.toggle_agent_suspend,
        (Action::RestartAgent, _) => &keys.restart_agent,
        (Action::HandoffCompact, _) => &keys.handoff_compact,
        (Action::ToggleInfoPane, _) => &keys.toggle_info_pane,
        (Action::Important, _) => &keys.toggle_tab_important,
        (Action::NewWorktree, _) => &keys.new_worktree,
        (Action::OpenWorktree, _) => &keys.open_worktree,
        (Action::RemoveWorktree, _) => &keys.remove_worktree,
        (Action::RenamePane, _) => &keys.rename_pane,
        (Action::SplitRight, _) => &keys.split_vertical,
        (Action::SplitDown, _) => &keys.split_horizontal,
        (Action::Zoom, _) => &keys.zoom,
        (Action::ClosePane, _) => &keys.close_pane,
        (Action::NewsOpen, _) => &keys.open_news,
        (Action::BrowserOpenOverlay, _) => &keys.open_browser,
        (Action::Coordinator(super::coordinator::CoordinatorMenuAction::Focus), _) => {
            &keys.open_coordinator
        }
        _ => return None,
    })
}

/// The first binding of an item's action, compact (`⌥t`, `^b X`); `None`
/// when unbound (and for the selectors and disabled items).
pub(super) fn key_hint(
    target: &ClientContextMenuTarget,
    item: &ClientContextMenuItem,
    keybinds: &LiveKeybindConfig,
) -> Option<String> {
    if matches!(item.look.kind, MenuKind::Selector | MenuKind::Disabled) {
        return None;
    }
    let binding = item_binding(target, item.action, &keybinds.keybinds)?;
    let label = binding.labels().into_iter().next()?;
    compact_binding(&label, keybinds.primary_prefix())
}

/// What the hint column shows: `on` for a toggle that is on, else the key
/// hint. Only the key hint sizes the menu, so a toggle's state never
/// changes its width (an `on` that does not fit is left out; the dot still
/// shows the state).
fn shown_hint(item: &ClientContextMenuItem, key: Option<String>) -> Option<String> {
    if matches!(
        item.look.kind,
        MenuKind::Toggle(true) | MenuKind::Star(true)
    ) {
        Some(ON_HINT.to_owned())
    } else {
        key
    }
}

/// `alt+t` → `⌥t`, `prefix+shift+x` → `^b X` (the primary prefix compact).
pub(super) fn compact_binding(label: &str, prefix: Option<KeyCombo>) -> Option<String> {
    match label.strip_prefix("prefix+") {
        Some(rest) => {
            let combo = crate::config::parse_key_combo(rest)?;
            let prefix = prefix.unwrap_or((KeyCode::Char('b'), KeyModifiers::CONTROL));
            Some(format!(
                "{} {}",
                compact_combo(prefix),
                compact_combo(combo)
            ))
        }
        None => crate::config::parse_key_combo(label).map(compact_combo),
    }
}

/// One key combo in a few cells: `^` ctrl, `⌥` alt, `⌘` cmd (macOS), a
/// shifted letter upper case, `⇧` before any other shifted key; other
/// modifiers spelled out (`hyper+`).
fn compact_combo((code, modifiers): KeyCombo) -> String {
    let mut out = String::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        out.push('^');
    }
    if modifiers.contains(KeyModifiers::ALT) {
        out.push('\u{2325}');
    }
    if modifiers.contains(KeyModifiers::SUPER) {
        out.push_str(if cfg!(target_os = "macos") {
            "\u{2318}"
        } else {
            "super+"
        });
    }
    if modifiers.contains(KeyModifiers::HYPER) {
        out.push_str("hyper+");
    }
    if modifiers.contains(KeyModifiers::META) {
        out.push_str("meta+");
    }
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    if let KeyCode::Char(c) = code {
        if c.is_alphabetic() {
            if shift {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            return out;
        }
    }
    if shift || code == KeyCode::BackTab {
        out.push('\u{21E7}');
    }
    match code {
        KeyCode::Char(' ') => out.push_str("space"),
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("enter"),
        KeyCode::Esc => out.push_str("esc"),
        KeyCode::Tab | KeyCode::BackTab => out.push_str("tab"),
        KeyCode::Backspace => out.push_str("bksp"),
        KeyCode::Left => out.push('\u{2190}'),
        KeyCode::Right => out.push('\u{2192}'),
        KeyCode::Up => out.push('\u{2191}'),
        KeyCode::Down => out.push('\u{2193}'),
        KeyCode::F(n) => {
            use std::fmt::Write as _;
            let _ = write!(out, "f{n}");
        }
        code => {
            use std::fmt::Write as _;
            let _ = write!(out, "{}", format!("{code:?}").to_lowercase());
        }
    }
    out
}

/// Cells an item row needs between the borders.
fn item_width(item: &ClientContextMenuItem, hint: Option<&str>) -> u16 {
    match item.action {
        ClientContextMenuAction::RemindTop | ClientContextMenuAction::RemindBottom => {
            LABEL_X + super::tab_remind_menu::remind_row_width()
        }
        ClientContextMenuAction::Color => {
            LABEL_X + SELECTOR_LABEL_CELLS + super::tab_color::swatch_row_width()
        }
        _ => {
            LABEL_X
                + super::render::display_width(item.label)
                + hint.map_or(0, |hint| HINT_GAP + super::render::display_width(hint))
                + RIGHT_PAD
        }
    }
}

fn rule_width(section: MenuSection) -> u16 {
    match section {
        MenuSection::Titled(title) => super::render::display_width(title) + 5,
        MenuSection::Danger => 11,
        MenuSection::Lead | MenuSection::Last => 1,
    }
}

/// What a drawn menu registers for hit-testing.
#[derive(Default)]
pub(super) struct RenderedMenu {
    pub(super) area: Rect,
    /// One hit per item row, by item index (rule rows have none).
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) swatches: Vec<(Rect, usize)>,
    pub(super) remind_options: Vec<(Rect, usize)>,
}

/// Draw `menu` at its anchor, clamped to the screen.
pub(super) fn render_menu(
    buffer: &mut Buffer,
    menu: &ClientContextMenuOverlay,
    palette: &Palette,
    keybinds: &LiveKeybindConfig,
) -> Option<RenderedMenu> {
    let items = menu.items();
    let rows = menu_rows(&items);
    let keys: Vec<Option<String>> = items
        .iter()
        .map(|item| key_hint(&menu.target, item, keybinds))
        .collect();
    let screen = buffer.area;
    let inner_width = rows
        .iter()
        .map(|row| match row {
            MenuRow::Rule(section) => rule_width(*section),
            MenuRow::Item(index) => item_width(&items[*index], keys[*index].as_deref()),
        })
        .max()
        .unwrap_or(0);
    let width = inner_width
        .saturating_add(2)
        .clamp(MIN_WIDTH, MAX_WIDTH)
        .min(screen.width.max(1));
    let hints: Vec<Option<String>> = items
        .iter()
        .zip(keys)
        .map(|(item, key)| shown_hint(item, key))
        .collect();
    let height = (rows.len() as u16)
        .saturating_add(2)
        .min(screen.height.max(1));
    let x = menu
        .x
        .min(screen.x.saturating_add(screen.width.saturating_sub(width)));
    let y = menu.y.min(
        screen
            .y
            .saturating_add(screen.height.saturating_sub(height)),
    );
    let area = Rect::new(x, y, width, height);
    let inner = rounded_panel(buffer, area, palette)?;
    let mut rendered = RenderedMenu {
        area,
        ..RenderedMenu::default()
    };
    for (row_index, row) in rows.iter().enumerate() {
        let row_y = inner.y.saturating_add(row_index as u16);
        if row_y >= inner.bottom() {
            break;
        }
        let rect = Rect::new(inner.x, row_y, inner.width, 1);
        match *row {
            MenuRow::Rule(section) => draw_rule(buffer, rect, section, palette),
            MenuRow::Item(index) => {
                let item = &items[index];
                let highlighted = index == menu.highlighted;
                draw_item(
                    buffer,
                    rect,
                    menu,
                    item,
                    hints[index].as_deref(),
                    highlighted,
                    palette,
                    &mut rendered,
                );
                rendered.rows.push((rect, index));
            }
        }
    }
    Some(rendered)
}

/// The menu's box: rounded corners in accent over panel_bg.
fn rounded_panel(buffer: &mut Buffer, area: Rect, palette: &Palette) -> Option<Rect> {
    if area.width < 2 || area.height < 2 {
        return None;
    }
    let background = Style::default()
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    let border = background.fg(palette.accent);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            buffer[(x, y)].set_symbol(" ").set_style(background);
        }
    }
    let (left, right, top, bottom) = (area.x, area.right() - 1, area.y, area.bottom() - 1);
    for x in left..=right {
        let (top_symbol, bottom_symbol) = if x == left {
            ("\u{256D}", "\u{2570}")
        } else if x == right {
            ("\u{256E}", "\u{256F}")
        } else {
            ("\u{2500}", "\u{2500}")
        };
        buffer[(x, top)].set_symbol(top_symbol).set_style(border);
        buffer[(x, bottom)]
            .set_symbol(bottom_symbol)
            .set_style(border);
    }
    for y in top + 1..bottom {
        buffer[(left, y)].set_symbol("\u{2502}").set_style(border);
        buffer[(right, y)].set_symbol("\u{2502}").set_style(border);
    }
    Some(Rect::new(
        area.x + 1,
        area.y + 1,
        area.width - 2,
        area.height - 2,
    ))
}

/// `─ Title ─────` (title mauve, red for Danger; the line surface1) or an
/// untitled surface1 line.
fn draw_rule(buffer: &mut Buffer, rect: Rect, section: MenuSection, palette: &Palette) {
    let base = Style::default().bg(palette.panel_bg);
    let (title, title_fg) = match section {
        MenuSection::Titled(title) => (Some(title), palette.mauve),
        MenuSection::Danger => (Some("Danger"), palette.red),
        MenuSection::Lead | MenuSection::Last => (None, palette.surface1),
    };
    let mut x = rect.x;
    if let Some(title) = title {
        let head = format!("\u{2500} {title} ");
        x = super::render::put_segment(buffer, x, rect.y, rect.right(), &head, base.fg(title_fg));
    }
    let rest = rect.right().saturating_sub(x);
    let line = "\u{2500}".repeat(usize::from(rest));
    super::render::put_text(buffer, x, rect.y, rest, &line, base.fg(palette.surface1));
}

#[allow(clippy::too_many_arguments)] // one row's inputs; a struct would only rename them
fn draw_item(
    buffer: &mut Buffer,
    rect: Rect,
    menu: &ClientContextMenuOverlay,
    item: &ClientContextMenuItem,
    hint: Option<&str>,
    highlighted: bool,
    palette: &Palette,
    rendered: &mut RenderedMenu,
) {
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_style(rect, base);
    let glyph_x = rect.x.saturating_add(1);
    let label_x = rect.x.saturating_add(LABEL_X);
    match (item.action, &menu.target) {
        (
            ClientContextMenuAction::RemindTop | ClientContextMenuAction::RemindBottom,
            ClientContextMenuTarget::Tab { remind, .. },
        ) => {
            let row = usize::from(item.action == ClientContextMenuAction::RemindBottom);
            super::render::put_text(
                buffer,
                glyph_x,
                rect.y,
                1,
                item.look.glyph,
                base.fg(palette.accent),
            );
            // Only the option under the cursor wears the highlight.
            rendered
                .remind_options
                .extend(super::tab_remind_menu::render_remind_row(
                    buffer,
                    Rect::new(label_x, rect.y, rect.width.saturating_sub(LABEL_X), 1),
                    row,
                    *remind,
                    highlighted.then_some(remind.cursor),
                    palette,
                ));
            return;
        }
        (ClientContextMenuAction::Color, ClientContextMenuTarget::Tab { color, .. }) => {
            super::render::put_text(
                buffer,
                glyph_x,
                rect.y,
                1,
                item.look.glyph,
                base.fg(palette.accent),
            );
            super::render::put_text(
                buffer,
                label_x,
                rect.y,
                rect.width.saturating_sub(LABEL_X),
                COLOUR_LABEL,
                base.fg(palette.overlay1),
            );
            let swatch_x = label_x.saturating_add(SELECTOR_LABEL_CELLS);
            rendered.swatches = super::tab_color::render_swatches(
                buffer,
                Rect::new(swatch_x, rect.y, rect.right().saturating_sub(swatch_x), 1),
                color.current,
                highlighted.then_some(color.cursor),
                palette,
            );
            return;
        }
        _ => {}
    }
    let danger = item.look.kind == MenuKind::Danger;
    let disabled = item.look.kind == MenuKind::Disabled;
    let row_style = if highlighted {
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(if danger {
                palette.red
            } else if disabled {
                palette.surface1
            } else {
                palette.accent
            })
            .add_modifier(Modifier::BOLD)
    } else if danger {
        base.fg(palette.red)
    } else if disabled {
        base.fg(palette.overlay0)
    } else {
        base
    };
    let row_style = if highlighted && disabled {
        row_style.fg(palette.overlay1)
    } else {
        row_style
    };
    buffer.set_style(rect, row_style);
    let glyph_fg = match item.look.kind {
        _ if highlighted => row_style.fg.unwrap_or(palette.text),
        MenuKind::Action | MenuKind::Selector => palette.accent,
        MenuKind::Danger => palette.red,
        MenuKind::Disabled | MenuKind::Toggle(false) | MenuKind::Star(false) => palette.overlay0,
        MenuKind::Toggle(true) => palette.green,
        MenuKind::Star(true) => palette.yellow,
    };
    super::render::put_text(
        buffer,
        glyph_x,
        rect.y,
        1,
        item.look.glyph,
        row_style.fg(glyph_fg),
    );
    let label_room = rect.right().saturating_sub(label_x).saturating_sub(1);
    let label_width = super::render::display_width(item.label).min(label_room);
    super::render::put_truncated(buffer, label_x, rect.y, label_room, item.label, row_style);
    let Some(hint) = hint else {
        return;
    };
    let hint_width = super::render::display_width(hint);
    let hint_x = rect
        .right()
        .saturating_sub(RIGHT_PAD)
        .saturating_sub(hint_width);
    if hint_x < label_x.saturating_add(label_width).saturating_add(HINT_GAP) {
        // Too narrow for both: the label wins.
        return;
    }
    let hint_fg = if highlighted {
        row_style.fg.unwrap_or(palette.text)
    } else if hint == ON_HINT
        && matches!(
            item.look.kind,
            MenuKind::Toggle(true) | MenuKind::Star(true)
        )
    {
        palette.green
    } else {
        palette.overlay0
    };
    super::render::put_text(
        buffer,
        hint_x,
        rect.y,
        hint_width,
        hint,
        row_style.fg(hint_fg),
    );
}
