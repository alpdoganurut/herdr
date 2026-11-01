//! Fork (sidebar filters): the `tabs` sidebar's search field and
//! last-activity chip, both in the toolbar row, and (menu design) the
//! always-shown `◧` notes button between the chip and `+`, which opens or
//! closes the focused tab's info pane without taking the keyboard.
//!
//! Client-only TUI presentation state, never persisted and never sent to
//! the server: a restart (or a new client) starts with every tab listed, so
//! a filter can never hand back a truncated list nobody remembers setting.
//! Prior art: the legacy zellij sidebar's chips (`filters.rs` there), whose
//! rules carry over: the controls are toolbar chips, the filters compose by
//! AND, and one dim `~ N filtered` row at the end of the list clears them
//! all.
//!
//! - **Search** (`keys.sidebar_search`, default `prefix+f`, or a click on
//!   the field): while the field has the keyboard keys go to it, never to
//!   the pane (direct bindings and the prefix still fire first). The query's whitespace-separated terms must all match,
//!   case-insensitively, somewhere in the tab's label, its group's name or its
//!   agent kind. Esc clears the query and lets go; Enter focuses the first
//!   matching tab and lets go (the query stays).
//! - **Recency** (`◷` chip): off → 15m → 30m → 1h → 6h → 1d and round
//!   again; a left click steps forward, a shift-click or a right click back.
//!   It hides tabs whose last activity is older than the window. A tab with
//!   a working or blocked agent, or with subagents running, is active now;
//!   otherwise its last activity is the newest time one of its agents entered
//!   its current state (`endpoint.agent-times.v1`). A tab with no such time
//!   counts as old.
//!
//! The filtering itself happens in the sidebar model build
//! (`SidebarModel::rebuild`), which this state marks dirty on every change:
//! render reads the result. Fixed rows (Browser, News, coordinator) and the
//! Active / Pinned / Scheduled blocks ignore the filters, and the focused
//! tab always stays listed.
//!
//! The toolbar shows the controls only while the pointer is on the toolbar
//! row or a filter is engaged (field focused, a query, recency on), so the
//! toolbar of an unfiltered sidebar looks exactly as before. Their hit
//! rectangles are registered either way: a click on the empty toolbar
//! between the fold toggle and `+` focuses the field.

use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::render::{put_text, put_truncated};
use super::*;

/// The most search terms that take part (one bit each in a match mask);
/// later terms are ignored.
pub(crate) const MAX_TERMS: usize = 64;
/// The longest query, in characters; more typed or pasted text is dropped.
pub(crate) const MAX_QUERY_CHARS: usize = 256;
/// Cells of the recency chip (`◷ 15m`), right-aligned before `+`: the
/// glyph, a space (terminals that draw `◷` wide would otherwise run it into
/// the label) and up to three label cells.
const CHIP_CELLS: u16 = 5;
/// The field's fewest cells; a narrower toolbar draws no controls.
const MIN_FIELD_CELLS: u16 = 4;
/// The field's glyph.
pub(crate) const SEARCH_GLYPH: &str = "\u{2315}"; // ⌕
/// The recency chip's glyph.
pub(crate) const RECENCY_GLYPH: &str = "\u{25F7}"; // ◷
/// The notes button's glyph (the focused tab's info pane), the menus'
/// Info pane glyph.
pub(crate) const NOTES_GLYPH: &str = "\u{25E7}"; // ◧
const SEARCH_PLACEHOLDER: &str = "search";

/// The last-activity window, one value at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Recency {
    #[default]
    Off,
    M15,
    M30,
    H1,
    H6,
    D1,
}

impl Recency {
    /// The cycle in forward order; both directions wrap, so off is never
    /// more than one step from either end.
    pub(crate) const RING: [Recency; 6] = [
        Recency::Off,
        Recency::M15,
        Recency::M30,
        Recency::H1,
        Recency::H6,
        Recency::D1,
    ];

    /// The window; `None` while off.
    pub(crate) fn window(self) -> Option<Duration> {
        let minutes = match self {
            Recency::Off => return None,
            Recency::M15 => 15,
            Recency::M30 => 30,
            Recency::H1 => 60,
            Recency::H6 => 6 * 60,
            Recency::D1 => 24 * 60,
        };
        Some(Duration::from_secs(minutes * 60))
    }

    /// The chip's value text (empty while off).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Recency::Off => "",
            Recency::M15 => "15m",
            Recency::M30 => "30m",
            Recency::H1 => "1h",
            Recency::H6 => "6h",
            Recency::D1 => "1d",
        }
    }

    /// One step around `RING`.
    pub(crate) fn step(self, forward: bool) -> Self {
        let len = Self::RING.len();
        let index = Self::RING
            .iter()
            .position(|value| *value == self)
            .unwrap_or(0);
        let next = if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        Self::RING[next]
    }
}

/// The sidebar's filter state (client-only, not persisted).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarFilter {
    /// The field's text as typed.
    query: String,
    /// `query` lowercased and split on whitespace (at most `MAX_TERMS`).
    terms: Vec<String>,
    /// The field has the keyboard.
    pub(crate) focused: bool,
    pub(crate) recency: Recency,
}

impl SidebarFilter {
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn terms(&self) -> &[String] {
        &self.terms
    }

    /// Some filter narrows the list.
    pub(crate) fn active(&self) -> bool {
        !self.terms.is_empty() || self.recency != Recency::Off
    }

    /// The toolbar shows the controls without a hover: the field is
    /// focused or some filter is set.
    pub(crate) fn engaged(&self) -> bool {
        self.focused || !self.query.is_empty() || self.recency != Recency::Off
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        self.query.clear();
        self.query.push_str(query);
        self.retokenize();
    }

    /// Append typed or pasted text (control characters, newlines included,
    /// become spaces).
    pub(crate) fn push_text(&mut self, text: &str) {
        let room = MAX_QUERY_CHARS.saturating_sub(self.query.chars().count());
        self.query.extend(
            text.chars()
                .take(room)
                .map(|c| if c.is_control() { ' ' } else { c }),
        );
        self.retokenize();
    }

    /// Delete the last character; whether there was one.
    pub(crate) fn pop_char(&mut self) -> bool {
        let popped = self.query.pop().is_some();
        if popped {
            self.retokenize();
        }
        popped
    }

    /// Delete the last word (and the spaces after it).
    pub(crate) fn pop_word(&mut self) {
        let kept = self.query.trim_end().len();
        let start = self.query[..kept]
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(index, c)| index + c.len_utf8());
        self.query.truncate(start);
        self.retokenize();
    }

    /// Drop every filter and let go of the keyboard.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    fn retokenize(&mut self) {
        self.terms.clear();
        self.terms.extend(
            self.query
                .split_whitespace()
                .take(MAX_TERMS)
                .map(str::to_lowercase),
        );
    }
}

/// Whether `haystack` contains `needle` (already lowercase), ignoring case.
/// No allocation; ASCII text compares bytes.
pub(crate) fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let needle = needle.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    haystack.char_indices().any(|(index, _)| {
        let mut lowered = haystack[index..].chars().flat_map(char::to_lowercase);
        needle.chars().all(|c| lowered.next() == Some(c))
    })
}

/// Bit `i` set when `terms[i]` occurs in `text`.
pub(crate) fn term_mask(text: &str, terms: &[String]) -> u64 {
    terms
        .iter()
        .take(MAX_TERMS)
        .enumerate()
        .filter(|(_, term)| contains_ci(text, term))
        .fold(0, |mask, (index, _)| mask | (1 << index))
}

/// The mask with every term's bit set.
pub(crate) fn full_mask(terms: &[String]) -> u64 {
    match terms.len().min(MAX_TERMS) {
        0 => 0,
        MAX_TERMS => u64::MAX,
        len => (1u64 << len) - 1,
    }
}

/// The toolbar's notes button: one cell, two blank cells before `+` (at
/// `right - 2`, render_toolbar), so a wide `◧` does not crowd it. `None`
/// when it would reach the fold toggle.
pub(super) fn notes_button_rect(content: Rect) -> Option<Rect> {
    let x = content.right().checked_sub(5)?;
    (x >= content.x.saturating_add(3) && content.height > 0).then(|| Rect::new(x, content.y, 1, 1))
}

/// The toolbar's filter controls for one frame: the field from after the
/// fold toggle to the chip, the chip right-aligned before the notes button
/// with one blank cell on each side. `None` when the toolbar is too narrow.
pub(super) fn control_rects(content: Rect) -> Option<(Rect, Rect)> {
    let y = content.y;
    // The notes button sits at `right - 5` (notes_button_rect); the chip
    // ends one cell before it.
    let chip_x = content.right().checked_sub(6 + CHIP_CELLS)?;
    let field_x = content.x.saturating_add(3);
    let field_width = chip_x.checked_sub(1)?.checked_sub(field_x)?;
    if field_width < MIN_FIELD_CELLS || content.height == 0 {
        return None;
    }
    Some((
        Rect::new(field_x, y, field_width, 1),
        Rect::new(chip_x, y, CHIP_CELLS, 1),
    ))
}

/// Draw the field and the chip into the toolbar row (when `reveal`) and
/// register their hits (with mouse capture), reading only `filter`.
pub(super) fn render_filter_controls(
    buffer: &mut Buffer,
    content: Rect,
    filter: &SidebarFilter,
    reveal: bool,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let Some((field, chip)) = control_rects(content) else {
        return;
    };
    if config.mouse_capture {
        hits.sidebar_toolbar = Rect::new(content.x, content.y, content.width, 1);
        hits.sidebar_search = field;
        hits.sidebar_recency = chip;
    }
    if !(reveal || filter.engaged()) {
        return;
    }
    let palette = &config.palette;
    let (x, y) = (field.x, field.y);
    let text_x = x.saturating_add(2);
    let text_width = field.width.saturating_sub(2);
    if filter.focused {
        buffer.set_style(field, Style::default().bg(palette.surface0));
        put_text(
            buffer,
            x,
            y,
            1,
            SEARCH_GLYPH,
            Style::default().fg(palette.accent),
        );
        // The tail of the query and a block caret after it.
        let room = usize::from(text_width.saturating_sub(1));
        let tail = query_tail(filter.query(), room);
        let tail_width = display_width(tail);
        put_text(
            buffer,
            text_x,
            y,
            text_width,
            tail,
            Style::default().fg(palette.text),
        );
        let caret_x = text_x.saturating_add(tail_width);
        if caret_x < field.right() {
            put_text(
                buffer,
                caret_x,
                y,
                1,
                " ",
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::REVERSED),
            );
        }
    } else if filter.query().is_empty() {
        let dim = Style::default().fg(palette.overlay0);
        put_text(buffer, x, y, 1, SEARCH_GLYPH, dim);
        put_truncated(buffer, text_x, y, text_width, SEARCH_PLACEHOLDER, dim);
    } else {
        put_text(
            buffer,
            x,
            y,
            1,
            SEARCH_GLYPH,
            Style::default().fg(palette.accent),
        );
        put_truncated(
            buffer,
            text_x,
            y,
            text_width,
            filter.query(),
            Style::default().fg(palette.subtext0),
        );
    }

    // The chip: `◷` dim while off, `◷ 1h` in accent while on, right-aligned.
    let label = filter.recency.label();
    let width = if label.is_empty() {
        1
    } else {
        2 + display_width(label)
    };
    let chip_x = chip.right().saturating_sub(width);
    let on = filter.recency != Recency::Off;
    let glyph_style = Style::default().fg(if on { palette.accent } else { palette.overlay0 });
    put_text(buffer, chip_x, y, 1, RECENCY_GLYPH, glyph_style);
    if on {
        put_text(
            buffer,
            chip_x.saturating_add(2),
            y,
            width.saturating_sub(2),
            label,
            Style::default().fg(palette.accent),
        );
    }
}

/// The `~ N filtered` row: dim, in the tab rows' icon column, with the
/// hover bar while hovered.
pub(super) fn render_filtered_row(
    buffer: &mut Buffer,
    rect: Rect,
    hidden: u16,
    hovered: bool,
    config: &ClientShellConfig,
) {
    use std::fmt::Write as _;
    const TEXT_X: u16 = 5;
    let palette = &config.palette;
    if hovered {
        buffer.set_style(rect, Style::default().bg(palette.sidebar_hover_bg()));
        put_text(
            buffer,
            rect.x,
            rect.y,
            1,
            super::tab_sidebar::HOVER_BAR,
            Style::default().fg(palette.accent),
        );
    }
    let mut text = super::sidebar_model::StackStr::<24>::new();
    let _ = write!(text, "~ {hidden} filtered");
    put_truncated(
        buffer,
        rect.x.saturating_add(TEXT_X),
        rect.y,
        rect.width.saturating_sub(TEXT_X + 1),
        text.as_str(),
        Style::default().fg(palette.overlay0),
    );
}

/// The longest tail of `text` that fits `cells` columns.
fn query_tail(text: &str, cells: usize) -> &str {
    let mut width = 0;
    let mut start = text.len();
    for (index, c) in text.char_indices().rev() {
        width += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width > cells {
            break;
        }
        start = index;
    }
    &text[start..]
}

fn display_width(text: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(text).min(usize::from(u16::MAX)) as u16
}

impl ClientShellState {
    /// The search field has the keyboard: it is focused and the expanded
    /// `tabs` sidebar of one machine draws it.
    pub(super) fn sidebar_search_focused(&self) -> bool {
        self.sidebar_filter.focused && self.sidebar_filter_shown()
    }

    /// The field lets go of the keyboard once its sidebar is gone (collapsed,
    /// another layout, several machines, no snapshot), so it never resumes
    /// focus on its own. Called at the top of compose and of key routing.
    pub(super) fn blur_hidden_sidebar_search(&mut self) {
        if self.sidebar_filter.focused && !self.sidebar_filter_shown() {
            self.sidebar_filter.focused = false;
        }
    }

    /// The expanded `tabs` sidebar (one machine) is up, so its toolbar is.
    fn sidebar_filter_shown(&self) -> bool {
        self.config.sidebar_layout == crate::config::SidebarLayoutConfig::Tabs
            && !self.sidebar_collapsed
            && self.endpoints.len() == 1
            && self.snapshot.is_some()
    }

    /// Give the field the keyboard (the info dock lets go of it).
    pub(super) fn focus_sidebar_search(&mut self, outcome: &mut ClientShellInput) {
        if !self.sidebar_filter_shown() {
            return;
        }
        self.info_dock_pane_pressed(outcome);
        self.sidebar_filter.focused = true;
        outcome.repaint = true;
    }

    /// `keys.sidebar_search`: focus the field, or let go of it.
    pub(super) fn toggle_sidebar_search(&mut self, outcome: &mut ClientShellInput) {
        if self.sidebar_search_focused() {
            self.blur_sidebar_search(outcome);
        } else {
            self.focus_sidebar_search(outcome);
        }
    }

    /// The field lets go of the keyboard (the query stays).
    pub(super) fn blur_sidebar_search(&mut self, outcome: &mut ClientShellInput) {
        if std::mem::take(&mut self.sidebar_filter.focused) {
            outcome.repaint = true;
        }
    }

    /// A filter changed: the list rebuilds from its top.
    fn sidebar_filter_changed(&mut self, outcome: &mut ClientShellInput) {
        self.sidebar_model.mark_dirty();
        self.agent_scroll = 0;
        self.sidebar_hover = None;
        outcome.repaint = true;
    }

    /// Drop both filters and let go of the keyboard (the `~ N filtered` row).
    pub(super) fn clear_sidebar_filters(&mut self, outcome: &mut ClientShellInput) {
        self.sidebar_filter.clear();
        self.sidebar_filter_changed(outcome);
    }

    /// Step the recency chip.
    pub(super) fn step_sidebar_recency(&mut self, forward: bool, outcome: &mut ClientShellInput) {
        self.sidebar_filter.recency = self.sidebar_filter.recency.step(forward);
        self.sidebar_filter_changed(outcome);
    }

    /// A key while the field has the keyboard. Nothing reaches the pane.
    pub(super) fn handle_sidebar_search_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        use crossterm::event::{KeyCode, KeyModifiers};
        outcome.repaint = true;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Esc => {
                self.sidebar_filter.set_query("");
                self.sidebar_filter.focused = false;
                self.sidebar_filter_changed(outcome);
            }
            KeyCode::Enter => {
                self.sidebar_filter.focused = false;
                self.jump_to_first_sidebar_match(outcome);
            }
            KeyCode::Backspace if ctrl || key.modifiers.contains(KeyModifiers::ALT) => {
                self.sidebar_filter.pop_word();
                self.sidebar_filter_changed(outcome);
            }
            KeyCode::Backspace => {
                if self.sidebar_filter.pop_char() {
                    self.sidebar_filter_changed(outcome);
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.sidebar_filter.set_query("");
                self.sidebar_filter_changed(outcome);
            }
            KeyCode::Char('w') if ctrl => {
                self.sidebar_filter.pop_word();
                self.sidebar_filter_changed(outcome);
            }
            KeyCode::Char(c) if plain => {
                let text = key
                    .generated_text
                    .clone()
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| c.to_string());
                self.sidebar_filter.push_text(&text);
                self.sidebar_filter_changed(outcome);
            }
            _ => {}
        }
    }

    /// Typed (IME) or pasted text while the field has the keyboard.
    pub(super) fn sidebar_search_insert_text(
        &mut self,
        text: &str,
        outcome: &mut ClientShellInput,
    ) {
        self.sidebar_filter.push_text(text);
        self.sidebar_filter_changed(outcome);
    }

    /// Enter: focus the first matching list tab (the model is brought up to
    /// date first: keys of the same batch may have changed the query).
    fn jump_to_first_sidebar_match(&mut self, outcome: &mut ClientShellInput) {
        let news_row = self.news_row();
        let coordinator_row = self.coordinator_row();
        if !self.ensure_tabs_sidebar_model(news_row.as_ref(), coordinator_row.as_ref()) {
            return;
        }
        let tab_id = self.sidebar_model.first_match.and_then(|tab| {
            self.snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.tabs.get(tab as usize))
                .map(|tab| tab.tab_id.clone())
        });
        if let Some(tab_id) = tab_id {
            self.jump_to_sidebar_tab(tab_id, outcome);
        }
    }

    /// A left press on the toolbar's filter controls or the `~ N filtered`
    /// row. Returns whether it was theirs.
    pub(super) fn sidebar_filter_press(
        &mut self,
        point: (u16, u16),
        modifiers: crossterm::event::KeyModifiers,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if super::contains(self.hits.sidebar_filtered, point) {
            self.clear_sidebar_filters(outcome);
            return true;
        }
        if super::contains(self.hits.sidebar_notes, point) {
            // The focused tab's info pane, never taking the keyboard.
            self.toggle_info_pane_for_focused_tab(outcome);
            self.sidebar_hover = Some(super::sidebar_model::SidebarHover::Toolbar);
            outcome.repaint = true;
            return true;
        }
        if super::contains(self.hits.sidebar_recency, point) {
            let back = modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
            self.step_sidebar_recency(!back, outcome);
            // The pointer is still on the toolbar: keep its controls shown.
            self.sidebar_hover = Some(super::sidebar_model::SidebarHover::Toolbar);
            return true;
        }
        if super::contains(self.hits.sidebar_search, point) {
            self.focus_sidebar_search(outcome);
            return true;
        }
        false
    }

    /// A right press on the recency chip steps it back. Returns whether it
    /// was the chip's.
    pub(super) fn sidebar_filter_right_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !super::contains(self.hits.sidebar_recency, point) {
            return false;
        }
        self.step_sidebar_recency(false, outcome);
        true
    }

    /// Any left press away from the field lets go of the keyboard.
    pub(super) fn sidebar_search_pressed_elsewhere(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) {
        if self.sidebar_filter.focused && !super::contains(self.hits.sidebar_search, point) {
            self.blur_sidebar_search(outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recency_cycles_both_ways_and_wraps() {
        let mut value = Recency::Off;
        let mut seen = Vec::new();
        for _ in 0..Recency::RING.len() {
            value = value.step(true);
            seen.push(value);
        }
        assert_eq!(
            seen,
            [
                Recency::M15,
                Recency::M30,
                Recency::H1,
                Recency::H6,
                Recency::D1,
                Recency::Off
            ]
        );
        assert_eq!(Recency::Off.step(false), Recency::D1);
        assert_eq!(Recency::M15.step(false), Recency::Off);
        assert_eq!(Recency::Off.window(), None);
        assert_eq!(Recency::M30.window(), Some(Duration::from_secs(1800)));
        assert_eq!(Recency::D1.window(), Some(Duration::from_secs(86_400)));
        assert!(Recency::RING
            .iter()
            .all(|value| display_width(value.label()) < CHIP_CELLS));
    }

    #[test]
    fn case_insensitive_contains_without_allocation_rules() {
        assert!(contains_ci("Level Plan", "plan"));
        assert!(contains_ci("LEVEL", "eve"));
        assert!(!contains_ci("level", "plans"));
        assert!(contains_ci("anything", ""));
        assert!(contains_ci("Über Tab", "über"));
        assert!(contains_ci("ÇAĞRI", "çağ"));
        assert!(!contains_ci("über", "x"));
    }

    #[test]
    fn terms_are_lowercase_and_split_on_whitespace() {
        let mut filter = SidebarFilter::default();
        filter.push_text("  Foo\tbar\nBAZ ");
        assert_eq!(filter.terms(), ["foo", "bar", "baz"]);
        assert_eq!(filter.query(), "  Foo bar BAZ ", "controls become spaces");
        assert!(filter.active());
        filter.pop_word();
        assert_eq!(filter.query(), "  Foo bar ");
        assert!(filter.pop_char());
        assert_eq!(filter.terms(), ["foo", "bar"]);
        filter.set_query("");
        assert!(!filter.active() && !filter.engaged());
        assert!(!filter.pop_char());
        let mask = term_mask("foo-bar", &["bar".into(), "foo".into(), "x".into()]);
        assert_eq!(mask, 0b011);
        assert_eq!(full_mask(&["a".into(), "b".into(), "c".into()]), 0b111);
        let many: Vec<String> = (0..70).map(|index| index.to_string()).collect();
        assert_eq!(full_mask(&many), u64::MAX);
        assert_eq!(full_mask(&[]), 0);
    }

    #[test]
    fn pop_word_splits_on_unicode_whitespace() {
        for space in ['\u{a0}', '\u{3000}'] {
            let mut filter = SidebarFilter::default();
            filter.set_query(&format!("foo{space}bar"));
            filter.pop_word();
            assert_eq!(filter.query(), format!("foo{space}"));
            filter.pop_word();
            assert_eq!(filter.query(), "");
        }
    }

    #[test]
    fn the_query_is_capped_on_a_char_boundary() {
        let mut filter = SidebarFilter::default();
        filter.push_text(&"é".repeat(300));
        assert_eq!(filter.query().chars().count(), MAX_QUERY_CHARS);
        filter.push_text("x");
        assert_eq!(filter.query().chars().count(), MAX_QUERY_CHARS);
        assert!(filter.pop_char());
        filter.push_text("xy");
        assert!(filter.query().ends_with('x'));
    }

    #[test]
    fn query_tail_keeps_the_end_that_fits() {
        assert_eq!(query_tail("abcdef", 3), "def");
        assert_eq!(query_tail("ab", 5), "ab");
        assert_eq!(query_tail("abc", 0), "");
        assert_eq!(query_tail("a漢字", 3), "字");
    }

    #[test]
    fn control_rects_sit_between_the_toggle_and_the_notes_button() {
        let (field, chip) = control_rects(Rect::new(0, 0, 30, 20)).expect("fits");
        assert_eq!((field.x, field.width), (3, 15));
        assert_eq!((chip.x, chip.width), (19, CHIP_CELLS));
        let notes = notes_button_rect(Rect::new(0, 0, 30, 20)).expect("fits");
        assert_eq!((notes.x, notes.width), (25, 1));
        assert_eq!(chip.right() + 1, notes.x, "one blank cell between");
        assert!(control_rects(Rect::new(0, 0, 14, 20)).is_none());
        assert!(notes_button_rect(Rect::new(0, 0, 14, 20)).is_some());
        assert!(notes_button_rect(Rect::new(0, 0, 7, 20)).is_none());
    }
}
