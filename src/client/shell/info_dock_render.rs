//! Draws the info dock (fork) in the almanac's Dusk palette. Pure apart
//! from filling the dock's hit map: the rows come from the memoised model
//! (`info_dock_model`) and the scroll from `compute_view`.
//!
//! Column 0 is the divider (`│`, a gold `┃` while dragged), column 1 the
//! marker column (`›` beside the selected checkpoint, `▸` beside the
//! editor's line). Row 0 is the Notes / History strip; the last two rows
//! are a `┄` rule and the footer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::info_dock::InfoDockHits;
use super::info_dock_model::{Row, DUSK};

/// Rows the dock draws besides its scrolling body: the strip, the rule and
/// the footer.
pub(crate) const CHROME_ROWS: u16 = 3;

/// What one frame of the dock shows.
pub(crate) struct DockFrame<'a> {
    pub(crate) strip: &'a Row,
    /// Rows under the strip that do not scroll (the history chips).
    pub(crate) pinned: &'a [Row],
    /// The visible slice of the scrolling rows.
    pub(crate) body: &'a [Row],
    /// A marker beside one body row (an index into `body`).
    pub(crate) mark: Option<(usize, &'static str)>,
    pub(crate) footer: &'a Row,
    pub(crate) dragging: bool,
}

/// Draw `frame` into `area` and record what was drawn in `hits`.
pub(crate) fn render(
    buffer: &mut Buffer,
    area: Rect,
    frame: &DockFrame<'_>,
    hits: &mut InfoDockHits,
) {
    *hits = InfoDockHits::default();
    let area = area.intersection(buffer.area);
    if area.width < 3 || area.height < CHROME_ROWS + 1 {
        return;
    }
    hits.area = area;
    hits.divider = Rect::new(area.x, area.y, 1, area.height);
    hits.body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(CHROME_ROWS),
    );
    let page = Style::default().fg(DUSK.body).bg(DUSK.page);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(" ").set_style(page);
            }
        }
    }
    let (edge, edge_style) = if frame.dragging {
        (
            "┃",
            Style::default()
                .fg(DUSK.gold)
                .bg(DUSK.drag)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("│", Style::default().fg(DUSK.rule).bg(DUSK.page))
    };
    for y in area.top()..area.bottom() {
        buffer.set_string(area.x, y, edge, edge_style);
    }
    let content_x = area.x + 2;
    let content_width = area.width.saturating_sub(2);
    let last_body_y = area.bottom().saturating_sub(2);
    let mut y = area.y;
    draw_row(buffer, content_x, y, content_width, frame.strip, hits);
    y += 1;
    for row in frame.pinned {
        if y >= last_body_y {
            break;
        }
        draw_row(buffer, content_x, y, content_width, row, hits);
        y += 1;
    }
    for (index, row) in frame.body.iter().enumerate() {
        if y >= last_body_y {
            break;
        }
        if let Some((_, symbol)) = frame.mark.filter(|(at, _)| *at == index) {
            let style = Style::default()
                .fg(DUSK.rust)
                .bg(if symbol == "›" { DUSK.sel } else { DUSK.page })
                .add_modifier(Modifier::BOLD);
            buffer.set_string(area.x + 1, y, symbol, style);
        }
        draw_row(buffer, content_x, y, content_width, row, hits);
        y += 1;
    }
    let rule_y = area.bottom() - 2;
    buffer.set_stringn(
        content_x,
        rule_y,
        "┄".repeat(usize::from(content_width)),
        usize::from(content_width),
        Style::default().fg(DUSK.rule).bg(DUSK.page),
    );
    draw_row(
        buffer,
        content_x,
        area.bottom() - 1,
        content_width,
        frame.footer,
        hits,
    );
}

/// One row: its own target over the marker and content columns first, then
/// every drawn segment's target, so a segment beats its row.
fn draw_row(buffer: &mut Buffer, x: u16, y: u16, width: u16, row: &Row, hits: &mut InfoDockHits) {
    if let Some(target) = row.target.as_ref() {
        hits.targets.push((
            Rect::new(x.saturating_sub(1), y, width.saturating_add(1), 1),
            target.clone(),
        ));
    }
    let right = x.saturating_add(width);
    let mut cursor = x;
    for seg in &row.segs {
        if cursor >= right {
            break;
        }
        let room = usize::from(right - cursor);
        let (end, _) = buffer.set_stringn(cursor, y, &seg.text, room, seg.style);
        if let Some(target) = seg.target.as_ref() {
            if end > cursor {
                hits.targets
                    .push((Rect::new(cursor, y, end - cursor, 1), target.clone()));
            }
        }
        cursor = end;
    }
}
