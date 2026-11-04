//! Fork (scrollback scroll): host scroll regions for the client blit.
//!
//! When a pane's rows shift between two frames (herdr's own scrollback view
//! moving under the wheel, or output scrolling a pane), a cell diff rewrites
//! every row of the pane: ~20 KB per frame on a large screen, which a busy
//! terminal takes tens of milliseconds to accept. The blit instead moves the
//! region on the host (DECSTBM top/bottom margins, plus DECSLRM left/right
//! margins for a region narrower than the screen, then SU/SD) and diffs the
//! new frame against the old one as the host now shows it, so only the
//! exposed rows and whatever really changed are written.
//!
//! The diff models the host exactly: cells inside a scrolled region read the
//! old row they came from, exposed rows count as unknown and are always
//! written. A region is only scrolled when that saves rewriting at least two
//! of its rows, and never when a wide character straddles its left or right edge.

use std::io::Write;

use crate::protocol::{CellData, FrameData, SurfaceRect};

/// What the host terminal can scroll.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HostScrollSupport {
    /// No host scrolling (no probe ran, e.g. Windows): plain cell diffs.
    #[default]
    Off,
    /// DECSTBM only: regions spanning the full screen width.
    FullWidth,
    /// DECSTBM and DECSLRM (the host reported DECLRMM, mode 69): any rectangle.
    Margins,
}

/// The probe's verdict: DECSTBM is universal once the host answered DA1;
/// DECSLRM needs a DECRPM for mode 69 that says the mode is known (set or reset).
pub(crate) fn support_from_probe(
    answered_device_attributes: bool,
    left_right_margin_status: Option<u8>,
) -> HostScrollSupport {
    match (answered_device_attributes, left_right_margin_status) {
        (false, _) => HostScrollSupport::Off,
        (true, Some(1 | 2)) => HostScrollSupport::Margins,
        (true, _) => HostScrollSupport::FullWidth,
    }
}

/// One region moved on the host. A positive `shift` moves content up (SU):
/// row `y` then shows the previous row `y + shift`, as in `surface_scroll`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostScroll {
    pub(crate) rect: SurfaceRect,
    pub(crate) shift: i16,
}

// Panes per screen are few; a frame with more candidate regions plans the first ones.
const MAX_REGIONS: usize = 32;
// Below this many changed rows a region's plain diff is already small.
const MIN_CHANGED_ROWS: usize = 3;

fn rect_fits(rect: SurfaceRect, frame: &FrameData) -> bool {
    let full_width = rect.x == 0 && rect.width == frame.width;
    // DECSLRM ignores left >= right (libghostty-vt setLeftAndRightMargin), so a
    // one-column partial region would scroll its neighbours too.
    (rect.width >= 2 || (full_width && rect.width > 0))
        && rect.height >= 2
        && rect
            .x
            .checked_add(rect.width)
            .is_some_and(|end| end <= frame.width)
        && rect
            .y
            .checked_add(rect.height)
            .is_some_and(|end| end <= frame.height)
}

fn rects_overlap(a: SurfaceRect, b: SurfaceRect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn cell_at(frame: &FrameData, x: u16, y: u16) -> &CellData {
    &frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)]
}

/// A wide character crossing the left or right edge would be split by the
/// host scroll, so such a region is repainted instead.
fn straddles_wide_cell(frame: &FrameData, rect: SurfaceRect) -> bool {
    let right = rect.x + rect.width - 1;
    (rect.y..rect.y + rect.height).any(|y| {
        (rect.x > 0 && super::cell_width(cell_at(frame, rect.x - 1, y)) > 1)
            || super::cell_width(cell_at(frame, right, y)) > 1
            || cell_at(frame, rect.x, y).skip
    })
}

fn rect_row(frame: &FrameData, rect: SurfaceRect, y: u16) -> &[CellData] {
    let start = usize::from(y) * usize::from(frame.width) + usize::from(rect.x);
    &frame.cells[start..start + usize::from(rect.width)]
}

fn mix(hash: u64, value: u64) -> u64 {
    (hash.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95)
}

// Fingerprints only nominate a shift; the diff compares real cells, so a
// collision can cost bytes but never correctness.
fn row_hash(frame: &FrameData, rect: SurfaceRect, y: u16) -> u64 {
    let start = usize::from(y) * usize::from(frame.width) + usize::from(rect.x);
    frame.cells[start..start + usize::from(rect.width)]
        .iter()
        .fold(0, |hash, cell| {
            let hash = cell
                .symbol
                .bytes()
                .fold(hash, |hash, byte| mix(hash, u64::from(byte)));
            let hash = mix(hash, u64::from(cell.fg) << 32 | u64::from(cell.bg));
            mix(hash, u64::from(cell.modifier) | u64::from(cell.skip) << 16)
        })
}

/// The shift that best explains how `rect` changed, when it saves rewriting
/// at least two of the region's rows.
fn best_shift(prev: &FrameData, frame: &FrameData, rect: SurfaceRect) -> Option<i16> {
    let height = usize::from(rect.height);
    // Most diff frames change a row or two (a spinner, the cursor): count changed
    // rows by plain comparison and stop before hashing or allocating anything.
    let changed = (rect.y..rect.y + rect.height)
        .filter(|&y| rect_row(prev, rect, y) != rect_row(frame, rect, y))
        .take(MIN_CHANGED_ROWS)
        .count();
    if changed < MIN_CHANGED_ROWS {
        return None;
    }
    let old = (0..rect.height)
        .map(|y| row_hash(prev, rect, rect.y + y))
        .collect::<Vec<_>>();
    let new = (0..rect.height)
        .map(|y| row_hash(frame, rect, rect.y + y))
        .collect::<Vec<_>>();
    let matches = |shift: isize| {
        (0..height)
            .filter(|&y| {
                let source = y as isize + shift;
                source >= 0 && (source as usize) < height && new[y] == old[source as usize]
            })
            .count()
    };
    let unshifted = matches(0);
    let (mut best_shift, mut best) = (0isize, unshifted);
    for distance in 1..height {
        // A shift by `distance` can match at most `height - distance` rows.
        if height - distance <= best {
            break;
        }
        for shift in [distance as isize, -(distance as isize)] {
            let count = matches(shift);
            if count > best {
                (best_shift, best) = (shift, count);
            }
        }
    }
    // The scroll itself costs a few dozen bytes; two saved rows already pay for it.
    (best_shift != 0 && best >= unshifted + 2)
        .then(|| i16::try_from(best_shift).ok())
        .flatten()
}

/// Plans the host scrolls that turn `prev` (what the host shows) toward
/// `frame` inside the candidate `regions` (pane content rectangles).
pub(super) fn plan(
    prev: &FrameData,
    frame: &FrameData,
    regions: &[SurfaceRect],
    support: HostScrollSupport,
) -> Vec<HostScroll> {
    let cells = usize::from(frame.width) * usize::from(frame.height);
    if support == HostScrollSupport::Off
        || regions.is_empty()
        || prev.width != frame.width
        || prev.height != frame.height
        || prev.cells.len() != cells
        || frame.cells.len() != cells
    {
        return Vec::new();
    }
    let mut scrolls: Vec<HostScroll> = Vec::new();
    for &rect in regions.iter().take(MAX_REGIONS) {
        if !rect_fits(rect, frame)
            || (support == HostScrollSupport::FullWidth
                && (rect.x != 0 || rect.width != frame.width))
            || scrolls
                .iter()
                .any(|scroll| rects_overlap(scroll.rect, rect))
            || straddles_wide_cell(prev, rect)
        {
            continue;
        }
        if let Some(shift) = best_shift(prev, frame, rect) {
            scrolls.push(HostScroll { rect, shift });
        }
    }
    scrolls
}

/// Writes the host scroll sequences. Margins are restored after each region,
/// so the rest of the frame (and every later frame) paints on a full screen.
pub(super) fn write_scrolls(writer: &mut impl Write, scrolls: &[HostScroll], screen_width: u16) {
    if scrolls.is_empty() {
        return;
    }
    // Scrolled-in rows take the current background (BCE); start from the default.
    let _ = writer.write_all(b"\x1b[m");
    for scroll in scrolls {
        let rect = scroll.rect;
        let partial = rect.x != 0 || rect.width != screen_width;
        if partial {
            let _ = writer.write_all(b"\x1b[?69h");
        }
        let _ = write!(writer, "\x1b[{};{}r", rect.y + 1, rect.y + rect.height);
        if partial {
            let _ = write!(writer, "\x1b[{};{}s", rect.x + 1, rect.x + rect.width);
        }
        let distance = scroll.shift.unsigned_abs();
        let _ = if scroll.shift > 0 {
            write!(writer, "\x1b[{distance}S")
        } else {
            write!(writer, "\x1b[{distance}T")
        };
        if partial {
            // With DECLRMM set, a bare `CSI s` resets the left/right margins.
            let _ = writer.write_all(b"\x1b[s\x1b[?69l");
        }
        let _ = writer.write_all(b"\x1b[r");
    }
}

/// Clears any left/right and top/bottom margins a torn scroll frame left
/// behind; a full redraw and the terminal restore start from it.
pub(crate) fn write_margin_reset(writer: &mut impl Write) {
    let _ = writer.write_all(MARGIN_RESET);
}

pub(crate) const MARGIN_RESET: &[u8] = b"\x1b[?69l\x1b[r";

/// The old cell the host shows at (`x`, `y`) after `scrolls`, or `None` for
/// an exposed cell whose content is unknown and must be written.
pub(super) fn host_cell<'a>(
    prev: &'a FrameData,
    scrolls: &[HostScroll],
    x: u16,
    y: u16,
) -> Option<&'a CellData> {
    for scroll in scrolls {
        let rect = scroll.rect;
        if x < rect.x || x >= rect.x + rect.width || y < rect.y || y >= rect.y + rect.height {
            continue;
        }
        let source = i32::from(y) + i32::from(scroll.shift);
        if source < i32::from(rect.y) || source >= i32::from(rect.y + rect.height) {
            return None;
        }
        return Some(cell_at(prev, x, source as u16));
    }
    Some(cell_at(prev, x, y))
}

#[cfg(test)]
mod tests {
    use super::super::BlitEncoder;
    use super::*;

    fn cell(symbol: &str, fg: u32) -> CellData {
        CellData {
            symbol: symbol.to_owned(),
            fg,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn rect(x: u16, y: u16, width: u16, height: u16) -> SurfaceRect {
        SurfaceRect {
            x,
            y,
            width,
            height,
        }
    }

    /// A screen with a sidebar column of letters and one or more panes
    /// showing numbered lines starting at `tops[i]`.
    fn screen(width: u16, height: u16, panes: &[(SurfaceRect, usize)]) -> FrameData {
        let mut cells = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let mut symbol = if x == 0 {
                    ((b'a' + (y % 26) as u8) as char).to_string()
                } else {
                    " ".to_owned()
                };
                let mut fg = 0;
                for &(pane, top) in panes {
                    if x >= pane.x
                        && x < pane.x + pane.width
                        && y >= pane.y
                        && y < pane.y + pane.height
                    {
                        let line = format!("{:05}|line", top + usize::from(y - pane.y));
                        let column = usize::from(x - pane.x);
                        symbol = line.chars().nth(column).unwrap_or(' ').to_string();
                        fg = 1 + (top + usize::from(y - pane.y)) as u32 % 5;
                    }
                }
                cells.push(cell(&symbol, fg));
            }
        }
        FrameData {
            cells,
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    /// Paints `prev`, then the encoder's next frame, into libghostty-vt (the
    /// VT core real Ghostty runs) and returns the second frame's bytes.
    fn replay(
        prev: &FrameData,
        frame: &FrameData,
        regions: &[SurfaceRect],
        support: HostScrollSupport,
    ) -> (Vec<u8>, crate::ghostty::Terminal) {
        let mut terminal = crate::ghostty::Terminal::new(prev.width, prev.height, 0).unwrap();
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(prev, false);
        terminal.write(&initial.bytes);
        encoder.commit(prev.clone(), initial);
        let next = encoder.encode_scrolling(frame, false, false, regions, support);
        terminal.write(&next.bytes);
        (next.bytes, terminal)
    }

    fn assert_screen_matches(terminal: &crate::ghostty::Terminal, frame: &FrameData) {
        for y in 0..frame.height {
            for x in 0..frame.width {
                let (_, graphemes) = terminal.screen_cell(x, u32::from(y)).unwrap();
                let shown = graphemes
                    .iter()
                    .filter_map(|&code| char::from_u32(code))
                    .collect::<String>();
                let shown = if shown.is_empty() {
                    " ".to_owned()
                } else {
                    shown
                };
                assert_eq!(
                    shown,
                    cell_at(frame, x, y).symbol,
                    "cell ({x}, {y}) differs from the frame"
                );
            }
        }
    }

    fn plain_diff_len(prev: &FrameData, frame: &FrameData) -> usize {
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(prev, false);
        encoder.commit(prev.clone(), initial);
        encoder.encode(frame, false).bytes.len()
    }

    #[test]
    fn fork_smoke_a_scrolled_pane_beside_the_sidebar_replays_exactly_and_writes_less() {
        let pane = rect(3, 1, 20, 10);
        for (from, to) in [(100, 103), (103, 100), (100, 104)] {
            let prev = screen(24, 12, &[(pane, from)]);
            let frame = screen(24, 12, &[(pane, to)]);
            let (bytes, terminal) = replay(&prev, &frame, &[pane], HostScrollSupport::Margins);
            assert_screen_matches(&terminal, &frame);
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                text.contains("\x1b[?69h") && text.contains("\x1b[4;23s"),
                "{text:?}"
            );
            assert!(bytes.len() < plain_diff_len(&prev, &frame), "{from}->{to}");
        }
    }

    #[test]
    fn fork_smoke_host_margins_are_restored_after_the_scroll() {
        let pane = rect(3, 1, 20, 10);
        let prev = screen(24, 12, &[(pane, 100)]);
        let frame = screen(24, 12, &[(pane, 103)]);
        let (_, mut terminal) = replay(&prev, &frame, &[pane], HostScrollSupport::Margins);
        // A later full-screen scroll must move the sidebar too: no margin survived.
        terminal.write(b"\x1b[1S");
        let (_, graphemes) = terminal.screen_cell(0, 0).unwrap();
        assert_eq!(graphemes, vec![u32::from('b')]);
        let (_, graphemes) = terminal.screen_cell(3, 0).unwrap();
        assert_eq!(graphemes, vec![u32::from('0')]);
    }

    #[test]
    fn two_side_by_side_panes_scroll_independently() {
        let left = rect(2, 0, 10, 9);
        let right = rect(13, 0, 11, 9);
        let prev = screen(24, 9, &[(left, 10), (right, 50)]);
        let frame = screen(24, 9, &[(left, 13), (right, 48)]);
        let (bytes, terminal) = replay(&prev, &frame, &[left, right], HostScrollSupport::Margins);
        assert_screen_matches(&terminal, &frame);
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\x1b[3S") && text.contains("\x1b[2T"),
            "{text:?}"
        );
    }

    #[test]
    fn without_margins_only_a_full_width_region_scrolls() {
        let pane = rect(3, 1, 21, 10);
        let prev = screen(24, 12, &[(pane, 100)]);
        let frame = screen(24, 12, &[(pane, 103)]);
        let (bytes, terminal) = replay(&prev, &frame, &[pane], HostScrollSupport::FullWidth);
        assert_screen_matches(&terminal, &frame);
        assert!(!String::from_utf8_lossy(&bytes).contains("\x1b[r"));

        let wide = rect(0, 1, 24, 10);
        let prev = screen(24, 12, &[(wide, 100)]);
        let frame = screen(24, 12, &[(wide, 103)]);
        let (bytes, terminal) = replay(&prev, &frame, &[wide], HostScrollSupport::FullWidth);
        assert_screen_matches(&terminal, &frame);
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\x1b[2;11r\x1b[3S\x1b[r") && !text.contains("?69"),
            "{text:?}"
        );
    }

    #[test]
    fn off_support_writes_the_plain_diff() {
        let pane = rect(3, 1, 20, 10);
        let prev = screen(24, 12, &[(pane, 100)]);
        let frame = screen(24, 12, &[(pane, 103)]);
        let (bytes, terminal) = replay(&prev, &frame, &[pane], HostScrollSupport::Off);
        assert_screen_matches(&terminal, &frame);
        assert_eq!(bytes.len(), plain_diff_len(&prev, &frame));
    }

    #[test]
    fn a_wide_character_on_the_region_edge_blocks_the_scroll() {
        let pane = rect(3, 1, 20, 10);
        let mut prev = screen(24, 12, &[(pane, 100)]);
        let frame = screen(24, 12, &[(pane, 103)]);
        let index = usize::from(5u16) * 24 + 22;
        prev.cells[index] = cell("💡", 0);
        assert!(plan(&prev, &frame, &[pane], HostScrollSupport::Margins).is_empty());
        prev.cells[index] = cell("x", 0);
        assert_eq!(
            plan(&prev, &frame, &[pane], HostScrollSupport::Margins),
            vec![HostScroll {
                rect: pane,
                shift: 3
            }]
        );
    }

    #[test]
    fn an_edit_that_is_not_a_shift_plans_nothing() {
        let pane = rect(3, 1, 20, 10);
        let prev = screen(24, 12, &[(pane, 100)]);
        let mut frame = prev.clone();
        for x in 3..23 {
            frame.cells[usize::from(4u16) * 24 + x] = cell("z", 2);
        }
        assert!(plan(&prev, &frame, &[pane], HostScrollSupport::Margins).is_empty());
        let unrelated = screen(24, 12, &[(pane, 900)]);
        assert!(plan(&prev, &unrelated, &[pane], HostScrollSupport::Margins).is_empty());
    }

    #[test]
    fn exposed_rows_are_always_written_even_when_blank() {
        let pane = rect(3, 1, 20, 10);
        let prev = screen(24, 12, &[(pane, 100)]);
        let mut frame = screen(24, 12, &[(pane, 103)]);
        // The scrolled-in rows are blank in the new frame.
        for y in 8..11u16 {
            for x in 3..23u16 {
                frame.cells[usize::from(y) * 24 + usize::from(x)] = cell(" ", 0);
            }
        }
        let (_, terminal) = replay(&prev, &frame, &[pane], HostScrollSupport::Margins);
        assert_screen_matches(&terminal, &frame);
    }

    /// Every row differs from its neighbours, so a scroll that touches the
    /// wrong columns shows up in the replay.
    fn striped(width: u16, height: u16, column: u16, offset: u8) -> FrameData {
        let mut cells = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let symbol = if x == column {
                    (b'A' + (y as u8 + offset) % 26) as char
                } else {
                    (b'a' + (y as u8 * 7 + x as u8) % 26) as char
                };
                cells.push(cell(&symbol.to_string(), 0));
            }
        }
        FrameData {
            cells,
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    #[test]
    fn fork_smoke_a_one_column_partial_region_never_scrolls_its_neighbours() {
        let column = rect(5, 0, 1, 10);
        let prev = striped(12, 10, 5, 0);
        let frame = striped(12, 10, 5, 3);
        assert!(plan(&prev, &frame, &[column], HostScrollSupport::Margins).is_empty());
        let (bytes, terminal) = replay(&prev, &frame, &[column], HostScrollSupport::Margins);
        assert_screen_matches(&terminal, &frame);
        assert!(!String::from_utf8_lossy(&bytes).contains("?69h"));
        // Two columns are enough for DECSLRM, and the neighbours stay put.
        let pair = rect(5, 0, 2, 10);
        let mut prev = striped(12, 10, 5, 0);
        let mut frame = striped(12, 10, 5, 3);
        for y in 0..10u16 {
            let at = |y: u16| usize::from(y) * 12 + 6;
            prev.cells[at(y)] = cell(&((b'A' + y as u8 % 26) as char).to_string(), 1);
            frame.cells[at(y)] = cell(&((b'A' + (y as u8 + 3) % 26) as char).to_string(), 1);
        }
        let (bytes, terminal) = replay(&prev, &frame, &[pair], HostScrollSupport::Margins);
        assert_screen_matches(&terminal, &frame);
        assert!(String::from_utf8_lossy(&bytes).contains("\x1b[6;7s"));
    }

    #[test]
    fn an_unchanged_or_lightly_edited_region_is_not_hashed_into_a_shift() {
        let pane = rect(3, 1, 20, 10);
        let prev = screen(24, 12, &[(pane, 100)]);
        let mut frame = prev.clone();
        assert_eq!(best_shift(&prev, &frame, pane), None);
        for x in 3..23 {
            frame.cells[usize::from(2u16) * 24 + x] = cell("z", 2);
            frame.cells[usize::from(9u16) * 24 + x] = cell("y", 2);
        }
        assert_eq!(best_shift(&prev, &frame, pane), None);
    }

    #[test]
    fn fork_smoke_a_full_redraw_after_a_torn_write_clears_stale_margins() {
        let pane = rect(3, 1, 20, 10);
        let prev = screen(24, 12, &[(pane, 100)]);
        let frame = screen(24, 12, &[(pane, 200)]);
        let mut terminal = crate::ghostty::Terminal::new(24, 12, 0).unwrap();
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&prev, false);
        terminal.write(&initial.bytes);
        encoder.commit(prev.clone(), initial);
        // A scroll frame cut off after its margins were set.
        terminal.write(b"\x1b[?2026h\x1b[?69h\x1b[2;11r\x1b[4;23s");
        let repaint =
            encoder.encode_scrolling(&frame, true, false, &[pane], HostScrollSupport::Margins);
        assert!(repaint.full);
        assert!(String::from_utf8_lossy(&repaint.bytes).contains("\x1b[?69l\x1b[r"));
        terminal.write(&repaint.bytes);
        assert_screen_matches(&terminal, &frame);
        // And a later full-screen scroll moves every column again.
        terminal.write(b"\x1b[1S");
        let (_, graphemes) = terminal.screen_cell(0, 0).unwrap();
        assert_eq!(graphemes, vec![u32::from('b')]);
    }

    #[test]
    fn probe_verdicts() {
        assert_eq!(support_from_probe(false, Some(2)), HostScrollSupport::Off);
        assert_eq!(support_from_probe(true, None), HostScrollSupport::FullWidth);
        assert_eq!(
            support_from_probe(true, Some(0)),
            HostScrollSupport::FullWidth
        );
        assert_eq!(
            support_from_probe(true, Some(4)),
            HostScrollSupport::FullWidth
        );
        assert_eq!(
            support_from_probe(true, Some(2)),
            HostScrollSupport::Margins
        );
        assert_eq!(
            support_from_probe(true, Some(1)),
            HostScrollSupport::Margins
        );
    }
}
