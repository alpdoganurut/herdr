//! The Browser overlay (fork): one panel listing the herdr browser's
//! profiles and their tabs with who opened and last used each one. Keys:
//! Up/Down move, Enter focuses the tab's window (`browser.focus`), `s`
//! starts or stops the profile under the cursor, Esc closes. On a remote
//! endpoint the list is read-only.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::browser::{BrowserOverlayRow, ClientBrowserOverlay};
use super::Palette;

const WIDTH: u16 = 100;
const HEIGHT: u16 = 24;

fn centred(area: Rect, width: u16, height: u16) -> Option<Rect> {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    if width < 30 || height < 6 {
        return None;
    }
    Some(Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    ))
}

fn put(b: &mut Buffer, x: u16, y: u16, width: u16, spans: Vec<Span<'_>>, style: Style) {
    if width == 0 {
        return;
    }
    Paragraph::new(Line::from(spans))
        .style(style)
        .render(Rect::new(x, y, width, 1), b);
}

fn clip(text: &str, max: usize) -> String {
    crate::ui::truncate_end(text, max)
}

/// The rows the overlay draws, top to bottom, for `overlay.scroll`.
pub(crate) fn render_browser_overlay(
    b: &mut Buffer,
    overlay: &ClientBrowserOverlay,
    p: &Palette,
) -> Option<()> {
    let outer = centred(b.area, WIDTH, HEIGHT)?;
    let bg = Style::default().bg(p.panel_bg);
    for y in outer.y..outer.bottom() {
        for x in outer.x..outer.right() {
            b[(x, y)].set_symbol(" ").set_style(bg);
        }
    }
    // Border
    let border = Style::default().fg(p.accent).bg(p.panel_bg);
    for x in outer.x..outer.right() {
        b[(x, outer.y)].set_symbol("─").set_style(border);
        b[(x, outer.bottom() - 1)].set_symbol("─").set_style(border);
    }
    for y in outer.y..outer.bottom() {
        b[(outer.x, y)].set_symbol("│").set_style(border);
        b[(outer.right() - 1, y)].set_symbol("│").set_style(border);
    }
    b[(outer.x, outer.y)].set_symbol("╭");
    b[(outer.right() - 1, outer.y)].set_symbol("╮");
    b[(outer.x, outer.bottom() - 1)].set_symbol("╰");
    b[(outer.right() - 1, outer.bottom() - 1)].set_symbol("╯");
    let inner = Rect::new(
        outer.x + 2,
        outer.y + 1,
        outer.width.saturating_sub(4),
        outer.height.saturating_sub(2),
    );
    let title = Style::default()
        .fg(p.text)
        .bg(p.panel_bg)
        .add_modifier(Modifier::BOLD);
    put(
        b,
        inner.x,
        inner.y,
        inner.width,
        vec![Span::styled("browser", title)],
        bg,
    );
    let hint = if overlay.local {
        "↑↓ move · enter focus window · s start/stop · esc close"
    } else {
        "remote server: read-only · esc close"
    };
    let hint = clip(hint, inner.width as usize);
    put(
        b,
        inner.right().saturating_sub(hint.chars().count() as u16),
        inner.y,
        hint.chars().count() as u16,
        vec![Span::styled(
            hint.clone(),
            Style::default().fg(p.overlay1).bg(p.panel_bg),
        )],
        bg,
    );
    let list = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(2),
    );
    if overlay.loading && overlay.rows.is_empty() {
        put(
            b,
            list.x,
            list.y,
            list.width,
            vec![Span::styled(
                "loading…",
                Style::default().fg(p.overlay1).bg(p.panel_bg),
            )],
            bg,
        );
        return Some(());
    }
    let rows = &overlay.rows;
    let visible = usize::from(list.height);
    // Keep the cursor in view.
    let scroll = if overlay.cursor < overlay.scroll {
        overlay.cursor
    } else if overlay.cursor >= overlay.scroll + visible {
        overlay.cursor + 1 - visible
    } else {
        overlay.scroll
    };
    for (line, row) in rows.iter().skip(scroll).take(visible).enumerate() {
        let index = scroll + line;
        let y = list.y + line as u16;
        let selected = index == overlay.cursor;
        let row_style = if selected {
            Style::default().bg(p.active_row_bg)
        } else {
            bg
        };
        let spans =
            match row {
                BrowserOverlayRow::Profile {
                    name,
                    state,
                    running,
                } => vec![
                    Span::styled(
                        format!("{name} "),
                        Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("· {state}"),
                        Style::default().fg(if *running { p.accent } else { p.overlay1 }),
                    ),
                ],
                BrowserOverlayRow::Tab {
                    short,
                    title,
                    url,
                    opener,
                    last,
                    dialog,
                    active,
                    ..
                } => {
                    let width = list.width as usize;
                    let id_cells = 5;
                    let who_cells = 28.min(width / 3);
                    let url_cells = width.saturating_sub(id_cells + who_cells + 3);
                    let url_text = clip(
                        &display_url(url),
                        url_cells
                            .saturating_sub(title.chars().count().min(24) + 3)
                            .max(8),
                    );
                    let title_text = clip(title, 24);
                    let mut who = if last.is_empty() {
                        format!("opened by {opener}")
                    } else {
                        last.clone()
                    };
                    if *dialog {
                        who.push_str(" · dialog");
                    }
                    vec![
                        Span::styled(
                            format!("  {short:<4}"),
                            Style::default().fg(if *active { p.accent } else { p.subtext0 }),
                        ),
                        Span::styled(
                            clip(&who, who_cells),
                            Style::default().fg(if *dialog { p.red } else { p.subtext0 }),
                        ),
                        Span::raw(" ".repeat(
                            who_cells.saturating_sub(who.chars().count().min(who_cells)) + 1,
                        )),
                        Span::styled(title_text, Style::default().fg(p.text)),
                        Span::styled(format!("  {url_text}"), Style::default().fg(p.overlay1)),
                    ]
                }
                BrowserOverlayRow::Empty(text) => vec![Span::styled(
                    clip(text, list.width as usize),
                    Style::default().fg(p.overlay1),
                )],
            };
        put(b, list.x, y, list.width, spans, row_style);
    }
    Some(())
}

/// `github.com/foo/bar` for a URL.
fn display_url(url: &str) -> String {
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    stripped.strip_suffix('/').unwrap_or(stripped).to_string()
}
