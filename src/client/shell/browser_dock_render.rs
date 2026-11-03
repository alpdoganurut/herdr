//! Draws the browser band, the Browser view's area and the view strip
//! (fork, companion launcher). Pure apart from filling their hit map.
//!
//! The band and the area are the companion's: herdr fills them with the
//! companion's dark page colour (Dusk, like the info dock) and a centered
//! placeholder that is seen only while the companion is not over it. The
//! band's column 0 is its divider (`│`, a gold `┃` while dragged), as on
//! the info dock. The strip is one chrome row in the tab bar's colours:
//! `▣ Terminal  ◎ Browser · 3 … ◨`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::browser_dock::{
    BrowserDockHits, BrowserDockTarget, Placeholder, PlaceholderTone, ViewStrip,
};
use super::info_dock_model::DUSK;
use crate::app::state::Palette;

/// The browser marks (shared with the tab rows).
const MARK: &str = super::browser::TAB_BROWSER_MARKER;
const ATTENTION: &str = super::browser::TAB_BROWSER_ATTENTION_MARKER;
/// The strip's terminal segment and the dock toggle.
const TERMINAL_GLYPH: &str = "\u{25A3}"; // ▣
pub(crate) const DOCK_GLYPH: &str = "\u{25E8}"; // ◨
const STOPPED_GLYPH: &str = "\u{25CC}"; // ◌

fn width_of(text: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(text).min(usize::from(u16::MAX)) as u16
}

fn fill(buffer: &mut Buffer, area: Rect, style: Style) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(" ").set_style(style);
            }
        }
    }
}

/// `text` cut to `max` columns.
fn clip(text: &str, max: u16) -> String {
    let mut out = String::new();
    let mut used = 0u16;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if used + w > max {
            break;
        }
        used += w;
        out.push(ch);
    }
    out
}

/// Greedy word wrap to `width` columns (a word longer than a line is cut).
fn wrap(text: &str, width: u16) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if width_of(&candidate) <= width {
            line = candidate;
        } else {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            line = clip(word, width);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn tone_style(tone: PlaceholderTone) -> Style {
    let fg = match tone {
        PlaceholderTone::Title => DUSK.ink,
        PlaceholderTone::Body => DUSK.body,
        PlaceholderTone::Dim => DUSK.dim,
        PlaceholderTone::Error => DUSK.rust,
    };
    let style = Style::default().fg(fg).bg(DUSK.page);
    if tone == PlaceholderTone::Title {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// The placeholder, centered in `area` (the page colour is already there).
fn render_placeholder(
    buffer: &mut Buffer,
    area: Rect,
    placeholder: &Placeholder,
    hits: &mut BrowserDockHits,
) {
    if area.width < 4 || area.height < 1 {
        return;
    }
    let mid_y = area.y + area.height / 2;
    if placeholder.calm {
        let x = area.x + area.width / 2;
        buffer.set_string(x, mid_y, MARK, Style::default().fg(DUSK.rule).bg(DUSK.page));
        return;
    }
    let text_width = area.width.saturating_sub(4).max(1);
    let mut rows: Vec<(String, Style)> = Vec::new();
    for (index, (text, tone)) in placeholder.lines.iter().enumerate() {
        let mut text = text.clone();
        // The spinner leads the line after the heading (the state's).
        if let Some(spinner) = placeholder.spinner {
            if index == 1 {
                text = format!("{spinner} {text}");
            }
        }
        for line in wrap(&text, text_width) {
            rows.push((line, tone_style(*tone)));
        }
    }
    let button_rows = if placeholder.button.is_some() { 2 } else { 0 };
    let total = (rows.len() as u16).saturating_add(button_rows);
    let mut y = area.y + area.height.saturating_sub(total) / 2;
    for (index, (line, style)) in rows.iter().enumerate() {
        if y >= area.bottom() {
            return;
        }
        let width = width_of(line);
        let x = area.x + area.width.saturating_sub(width) / 2;
        // The heading's browser mark in the accent.
        if index == 0 && placeholder.spinner.is_some() {
            buffer.set_string(
                x.saturating_sub(2),
                y,
                MARK,
                Style::default().fg(DUSK.gold).bg(DUSK.page),
            );
        }
        buffer.set_string(x, y, line, *style);
        y += 1;
    }
    if let Some((label, target)) = placeholder.button {
        y += 1;
        if y < area.bottom() {
            let width = width_of(label) + 4;
            let x = area.x + area.width.saturating_sub(width) / 2;
            let edge = Style::default().fg(DUSK.dim).bg(DUSK.page);
            buffer.set_string(x, y, "[", edge);
            buffer.set_string(
                x + 2,
                y,
                label,
                Style::default()
                    .fg(DUSK.accent)
                    .bg(DUSK.page)
                    .add_modifier(Modifier::BOLD),
            );
            buffer.set_string(x + width - 1, y, "]", edge);
            hits.targets.push((Rect::new(x, y, width, 1), target));
        }
    }
    if let Some(target) = placeholder.click {
        hits.targets.insert(0, (area, target));
    }
}

/// The band: the divider in column 0, the page colour and the placeholder
/// in the rest. Fork (session header): with a `title`, row 0 is the title
/// bar, `◧ <title>` left and `⤢  ✕` flush right, the placeholder under it.
pub(crate) fn render_band(
    buffer: &mut Buffer,
    band: Rect,
    dragging: bool,
    title: Option<&str>,
    placeholder: &Placeholder,
    hits: &mut BrowserDockHits,
) {
    let band = band.intersection(buffer.area);
    if band.width < 2 || band.height == 0 {
        return;
    }
    hits.band = band;
    hits.divider = Rect::new(band.x, band.y, 1, band.height);
    fill(buffer, band, Style::default().fg(DUSK.body).bg(DUSK.page));
    let (edge, edge_style) = if dragging {
        (
            "\u{2503}", // ┃
            Style::default()
                .fg(DUSK.gold)
                .bg(DUSK.drag)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("\u{2502}", Style::default().fg(DUSK.rule).bg(DUSK.page)) // │
    };
    for y in band.top()..band.bottom() {
        buffer.set_string(band.x, y, edge, edge_style);
    }
    let mut area = Rect::new(band.x + 1, band.y, band.width - 1, band.height);
    if let Some(title) = title {
        render_title_bar(buffer, area, title, hits);
        area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
    }
    render_placeholder(buffer, area, placeholder, hits);
}

/// Fork (session header): the band's title bar on `area`'s first row.
fn render_title_bar(buffer: &mut Buffer, area: Rect, title: &str, hits: &mut BrowserDockHits) {
    const PROMOTE: &str = "\u{2922}"; // ⤢
    const CLOSE: &str = "\u{2715}"; // ✕
    if area.width < 4 || area.height == 0 {
        return;
    }
    let y = area.y;
    let x = area.x + 1;
    let right = area.right().saturating_sub(1);
    let page = |color: Color| Style::default().fg(color).bg(DUSK.page);
    buffer.set_string(
        x,
        y,
        super::session_header::BROWSER_DOCK_GLYPH,
        page(DUSK.accent),
    );
    // `⤢  ✕` with a one-cell margin.
    let actions_x = right.saturating_sub(4);
    let title_x = x + 2;
    if actions_x > title_x + 1 {
        let room = actions_x - title_x - 1;
        super::render::put_truncated(buffer, title_x, y, room, title, page(DUSK.ink));
        buffer.set_string(actions_x, y, PROMOTE, page(DUSK.body));
        hits.targets
            .push((Rect::new(actions_x, y, 1, 1), BrowserDockTarget::Promote));
        buffer.set_string(actions_x + 3, y, CLOSE, page(DUSK.body));
        hits.targets
            .push((Rect::new(actions_x + 3, y, 1, 1), BrowserDockTarget::Close));
    }
}

/// The Browser view's area: the page colour and the placeholder.
pub(crate) fn render_full(
    buffer: &mut Buffer,
    area: Rect,
    placeholder: &Placeholder,
    hits: &mut BrowserDockHits,
) {
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return;
    }
    hits.full = area;
    fill(buffer, area, Style::default().fg(DUSK.body).bg(DUSK.page));
    render_placeholder(buffer, area, placeholder, hits);
}

/// The view strip: `▣ Terminal`, `◎ Browser · N`, the stopped link and `◨`.
pub(crate) fn render_view_strip(
    buffer: &mut Buffer,
    area: Rect,
    strip: &ViewStrip,
    palette: &Palette,
    hits: &mut BrowserDockHits,
) {
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return;
    }
    let base = Style::default().fg(palette.overlay1).bg(palette.panel_bg);
    buffer.set_style(area, base);
    for x in area.left()..area.right() {
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_symbol(" ");
        }
    }
    let selected = Style::default()
        .fg(palette.text)
        .bg(palette.surface0)
        .add_modifier(Modifier::BOLD);
    let right_edge = area.right().saturating_sub(4);
    let mut x = area.x + 1;
    // ▣ Terminal
    let terminal = format!(" {TERMINAL_GLYPH} Terminal ");
    let terminal_width = width_of(&terminal);
    if x + terminal_width <= right_edge {
        buffer.set_string(
            x,
            area.y,
            &terminal,
            if strip.browser { base } else { selected },
        );
        hits.targets.push((
            Rect::new(x, area.y, terminal_width, 1),
            BrowserDockTarget::ViewTerminal,
        ));
        x += terminal_width + 1;
    }
    // ◎ Browser · N (◉ while a page needs the user; ◎ lit while an agent works)
    let (mark, mark_color) = if strip.attention {
        (ATTENTION, palette.yellow)
    } else if strip.lit {
        (MARK, palette.accent)
    } else if strip.browser {
        (MARK, palette.text)
    } else {
        (MARK, palette.overlay0)
    };
    let count = if strip.pages > 0 && strip.stopped.is_none() {
        format!(" · {}", strip.pages)
    } else {
        String::new()
    };
    let label = format!("Browser{count} ");
    let segment_width = 3 + width_of(&label);
    if x + segment_width <= right_edge {
        let style = if strip.browser { selected } else { base };
        buffer.set_string(x, area.y, " ", style);
        buffer.set_string(x + 1, area.y, mark, style.fg(mark_color));
        buffer.set_string(x + 2, area.y, " ", style);
        buffer.set_string(x + 3, area.y, &label, style);
        hits.targets.push((
            Rect::new(x, area.y, segment_width, 1),
            BrowserDockTarget::ViewBrowser,
        ));
        x += segment_width + 1;
    }
    // ◌ companion stopped · start
    if let Some(stopped) = strip.stopped {
        let text = format!("{STOPPED_GLYPH} {stopped}");
        let link = " · start";
        let width = width_of(&text) + width_of(link);
        if x + width <= right_edge {
            let dim = Style::default().fg(palette.overlay0).bg(palette.panel_bg);
            buffer.set_string(x, area.y, &text, dim);
            let link_x = x + width_of(&text);
            if stopped == "companion stopped" {
                buffer.set_string(
                    link_x,
                    area.y,
                    link,
                    Style::default()
                        .fg(palette.accent)
                        .bg(palette.panel_bg)
                        .add_modifier(Modifier::UNDERLINED),
                );
                hits.targets.push((
                    Rect::new(link_x + 3, area.y, width_of(link) - 3, 1),
                    BrowserDockTarget::Start,
                ));
            }
        }
    }
    // ◨ at the right end: accent while docked.
    if area.width >= 4 {
        let toggle_x = area.right() - 3;
        let color: Color = if strip.docked {
            palette.accent
        } else {
            palette.overlay1
        };
        buffer.set_string(
            toggle_x + 1,
            area.y,
            DOCK_GLYPH,
            Style::default().fg(color).bg(palette.panel_bg),
        );
        hits.targets.push((
            Rect::new(toggle_x, area.y, 3, 1),
            BrowserDockTarget::ToggleDock,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_keeps_words_and_cuts_a_word_longer_than_a_line() {
        assert_eq!(
            wrap("run herdr browser setup --companion in a terminal", 20),
            vec!["run herdr browser", "setup --companion in", "a terminal"]
        );
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd"]);
        assert!(wrap("", 10).is_empty());
    }
}
