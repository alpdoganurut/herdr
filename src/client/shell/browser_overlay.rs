//! The Browser overlay (fork): one panel listing the herdr browser's
//! profiles and their tabs with who opened and last used each one. Keys:
//! Up/Down move, Enter focuses the tab's window (`browser.focus`), `s`
//! starts or stops the profile under the cursor, Esc closes. On a remote
//! endpoint the list is read-only.
//!
//! Fork (companion launcher): the companion app's block leads (state, pid,
//! uptime, version, restarts, the log, an update, the last error) with
//! `[Focus] [Restart] [Stop]` chips for the mouse; Enter focuses it, `s`
//! starts or stops it, `r` restarts it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::browser::{BrowserOverlayRow, ClientBrowserOverlay, CompanionTone};
use super::Palette;
use crate::api::schema::BrowserCompanionAction;

/// A mouse target of the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserOverlayHit {
    /// A companion chip.
    Companion(BrowserCompanionAction),
    /// A selectable row (the index into the overlay's rows).
    Row(usize),
}

/// What a render registers for the mouse: the panel and its targets
/// (chips before rows, so the first match is the most specific).
#[derive(Debug, Default)]
pub(crate) struct BrowserOverlayRender {
    pub(crate) area: Rect,
    pub(crate) hits: Vec<(Rect, BrowserOverlayHit)>,
}

/// The companion line's label column (`companion  `): its notes align under the text.
const COMPANION_INDENT: u16 = 11;

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
) -> Option<BrowserOverlayRender> {
    let outer = centred(b.area, WIDTH, HEIGHT)?;
    let mut render = BrowserOverlayRender {
        area: outer,
        hits: Vec::new(),
    };
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
    let companion = overlay
        .rows
        .iter()
        .any(|row| matches!(row, BrowserOverlayRow::Companion(_)));
    let hint = if overlay.local && companion {
        "↑↓ move · enter focus · s start/stop · r restart · esc close"
    } else if overlay.local {
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
        return Some(render);
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
    // The actor/op/age column is as wide as its widest visible row (capped),
    // so the title follows it instead of a fixed gap.
    let who_cap = (list.width as usize / 2).max(8);
    let who_cells = rows
        .iter()
        .skip(scroll)
        .take(visible)
        .filter_map(|row| match row {
            BrowserOverlayRow::Tab { .. } => Some(who_text(row).chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(8)
        .clamp(8, who_cap);
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
                    dialog,
                    active,
                    ..
                } => {
                    let width = list.width as usize;
                    let id_cells = 5;
                    let url_cells = width.saturating_sub(id_cells + who_cells + 3);
                    let url_text = clip(
                        &display_url(url),
                        url_cells
                            .saturating_sub(title.chars().count().min(24) + 3)
                            .max(8),
                    );
                    let title_text = clip(title, 24);
                    let who = who_text(row);
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
                // Fork (companion launcher).
                BrowserOverlayRow::Companion(row) => {
                    let line = Rect::new(list.x, y, list.width, 1);
                    let chips_x = render_companion_chips(
                        b,
                        line,
                        &row.chips,
                        overlay.local,
                        row_style,
                        p,
                        &mut render.hits,
                    );
                    let text_width = chips_x.saturating_sub(list.x + 1);
                    let room = (text_width as usize).saturating_sub(COMPANION_INDENT as usize + 2);
                    put(
                        b,
                        list.x,
                        y,
                        text_width,
                        vec![
                            Span::styled(
                                "companion  ",
                                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled("● ", Style::default().fg(companion_color(&row.state, p))),
                            Span::styled(clip(&row.line, room), Style::default().fg(p.subtext0)),
                        ],
                        row_style,
                    );
                    if row_style != bg {
                        // the selection band runs under the chips too
                        for x in list.x + text_width..chips_x {
                            b[(x, y)].set_style(row_style);
                        }
                    }
                    render.hits.push((
                        Rect::new(list.x, y, text_width, 1),
                        BrowserOverlayHit::Row(index),
                    ));
                    continue;
                }
                BrowserOverlayRow::CompanionNote { text, tone } => {
                    let color = match tone {
                        CompanionTone::Dim => p.overlay1,
                        CompanionTone::Warn => p.yellow,
                        CompanionTone::Error => p.red,
                    };
                    vec![
                        Span::raw(" ".repeat(COMPANION_INDENT as usize)),
                        Span::styled(
                            clip(
                                text,
                                (list.width as usize).saturating_sub(COMPANION_INDENT as usize),
                            ),
                            Style::default().fg(color),
                        ),
                    ]
                }
            };
        if matches!(
            row,
            BrowserOverlayRow::Tab { .. } | BrowserOverlayRow::Profile { .. }
        ) {
            render.hits.push((
                Rect::new(list.x, y, list.width, 1),
                BrowserOverlayHit::Row(index),
            ));
        }
        put(b, list.x, y, list.width, spans, row_style);
    }
    Some(render)
}

/// The state dot's colour.
fn companion_color(state: &str, p: &Palette) -> ratatui::style::Color {
    match state {
        "running" | "external" => p.accent,
        "starting" | "reconnecting" | "crashed" | "stopping" => p.yellow,
        "failed" => p.red,
        _ => p.overlay0,
    }
}

/// The chips, right-aligned on `line` (`[Focus] [Restart] [Stop]`; a
/// remote endpoint shows none: read-only). Answers where they begin.
fn render_companion_chips(
    b: &mut Buffer,
    line: Rect,
    chips: &[super::browser::CompanionChip],
    local: bool,
    style: Style,
    p: &Palette,
    hits: &mut Vec<(Rect, BrowserOverlayHit)>,
) -> u16 {
    let mut right = line.right();
    if !local {
        return right;
    }
    for chip in chips.iter().rev() {
        let width = chip.label.chars().count() as u16 + 2;
        if right < line.x + width + COMPANION_INDENT + 12 {
            break;
        }
        let x = right - width;
        let label = if chip.action == BrowserCompanionAction::Stop {
            p.red
        } else {
            p.accent
        };
        put(
            b,
            x,
            line.y,
            width,
            vec![
                Span::styled("[", Style::default().fg(p.overlay1)),
                Span::styled(
                    chip.label,
                    Style::default().fg(label).add_modifier(Modifier::BOLD),
                ),
                Span::styled("]", Style::default().fg(p.overlay1)),
            ],
            style,
        );
        hits.push((
            Rect::new(x, line.y, width, 1),
            BrowserOverlayHit::Companion(chip.action),
        ));
        right = x.saturating_sub(1);
    }
    right
}

/// `github.com/foo/bar` for a URL.
fn display_url(url: &str) -> String {
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    stripped.strip_suffix('/').unwrap_or(stripped).to_string()
}

/// The actor/op/age column of a tab row (`opened by …` until something ran).
fn who_text(row: &BrowserOverlayRow) -> String {
    match row {
        BrowserOverlayRow::Tab {
            opener,
            last,
            dialog,
            ..
        } => {
            let mut who = if last.is_empty() {
                format!("opened by {opener}")
            } else {
                last.clone()
            };
            if *dialog {
                who.push_str(" · dialog");
            }
            who
        }
        _ => String::new(),
    }
}
