//! The info dock's row model (fork): pure functions that turn the notes page
//! into drawable rows (`notes_rows`, `editor_rows`, the strip and the
//! footer). The markdown rules follow the picked design
//! (`.local/prd/notes-format/sonnet-directed.html`).
//!
//! Rows are plain data (`Row` of `Seg`s with optional click targets); the
//! dock state memoises them and `info_dock_render` only draws them. Every
//! byte offset a target carries is a char boundary of the line it names.

use std::collections::HashSet;

use ratatui::style::{Color, Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::api::schema::notes::NotesAuthor;

/// What a click in the dock does; the last registered target under the
/// pointer wins (a segment beats its row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InfoDockTarget {
    /// A task marker, with the notes revision its row was built from.
    Task {
        line: usize,
        revision: String,
    },
    /// Text: edit at the clicked column of the segment starting at
    /// `byte_start` (`spaced`: a spaced small-caps heading).
    EditAt {
        line: usize,
        byte_start: usize,
        spaced: bool,
    },
    /// Past the text: edit at `byte` of `line`.
    EditEnd {
        line: usize,
        byte: usize,
    },
    ConflictReload,
    ConflictKeepMine,
    Retry,
    /// Fork (session header): the title bar's `✎`: edit the notes (or
    /// leave the editor).
    Edit,
    /// Fork (session header): the title bar's `✕`: close the dock.
    Close,
}

/// One drawn run of text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Seg {
    pub(crate) text: String,
    pub(crate) style: Style,
    pub(crate) target: Option<InfoDockTarget>,
}

/// One dock line.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Row {
    pub(crate) segs: Vec<Seg>,
    pub(crate) target: Option<InfoDockTarget>,
}

impl Row {
    fn new(segs: Vec<Seg>, target: Option<InfoDockTarget>) -> Self {
        Self { segs, target }
    }

    /// The row's text, for tests and width checks.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        self.segs.iter().map(|seg| seg.text.as_str()).collect()
    }
}

// ------------------------------------------------------------------ Dusk

/// The almanac's Dusk palette (`fixture.json` `palette`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dusk {
    pub(crate) page: Color,
    pub(crate) ink: Color,
    pub(crate) body: Color,
    pub(crate) dim: Color,
    pub(crate) rule: Color,
    pub(crate) accent: Color,
    pub(crate) gold: Color,
    pub(crate) rust: Color,
    pub(crate) sel: Color,
    /// `mix(body, dim, .5)`.
    pub(crate) note: Color,
    /// `mix(page, gold, .25)`: the divider while it is dragged.
    pub(crate) drag: Color,
}

pub(crate) const DUSK: Dusk = Dusk {
    page: Color::Rgb(0x17, 0x18, 0x1e),
    ink: Color::Rgb(0xcf, 0xd2, 0xde),
    body: Color::Rgb(0xa6, 0xaa, 0xbb),
    dim: Color::Rgb(0x6b, 0x6f, 0x80),
    rule: Color::Rgb(0x36, 0x39, 0x47),
    accent: Color::Rgb(0xaa, 0x6e, 0xff),
    gold: Color::Rgb(0xa5, 0xa9, 0xcf),
    rust: Color::Rgb(0xd0, 0x9a, 0x8a),
    sel: Color::Rgb(0x22, 0x24, 0x2e),
    note: Color::Rgb(136, 140, 158),
    drag: Color::Rgb(58, 60, 74),
};

fn fg(color: Color) -> Style {
    Style::default().fg(color).bg(DUSK.page)
}

fn fg_on(color: Color, bg: Option<Color>) -> Style {
    Style::default().fg(color).bg(bg.unwrap_or(DUSK.page))
}

fn bold(style: Style) -> Style {
    style.add_modifier(Modifier::BOLD)
}

fn italic(style: Style) -> Style {
    style.add_modifier(Modifier::ITALIC)
}

fn seg(text: impl Into<String>, style: Style) -> Seg {
    Seg {
        text: text.into(),
        style,
        target: None,
    }
}

fn hit(text: impl Into<String>, style: Style, target: InfoDockTarget) -> Seg {
    Seg {
        text: text.into(),
        style,
        target: Some(target),
    }
}

fn segs_width(segs: &[Seg]) -> usize {
    segs.iter().map(|seg| seg.text.width()).sum()
}

// ------------------------------------------------------------------ text

/// Untrusted text: drop C0/C1 controls (escape sequences cannot reach the
/// host terminal) except `\n`, and expand tabs to four spaces.
pub(crate) fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            ch if (ch as u32) < 0x20 || (0x7f..=0x9f).contains(&(ch as u32)) => {}
            ch => out.push(ch),
        }
    }
    out
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

/// Word wrap to `width` columns: whitespace runs collapse to one space and a
/// word longer than the width is cut.
pub(crate) fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.width() + 1 + word.width() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
        while current.width() > width {
            let cut = cut_at_width(&current, width);
            let rest = current.split_off(cut);
            lines.push(std::mem::replace(&mut current, rest));
        }
    }
    lines.push(current);
    lines
}

/// The byte offset where `text` first exceeds `width` columns (at least one
/// char, so a lone wide char never loops).
fn cut_at_width(text: &str, width: usize) -> usize {
    let mut used = 0;
    for (index, ch) in text.char_indices() {
        let w = char_width(ch);
        if used + w > width {
            return if index == 0 { ch.len_utf8() } else { index };
        }
        used += w;
    }
    text.len()
}

/// The largest char boundary of `text` at or below `index`.
pub(crate) fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The byte offset in `text` of the char under column `dx`.
fn byte_at_column(text: &str, dx: usize) -> usize {
    let mut used = 0;
    for (index, ch) in text.char_indices() {
        let w = char_width(ch);
        if used + w > dx {
            return index;
        }
        used += w;
    }
    text.len()
}

/// `S P A C E D` small caps.
pub(crate) fn spaced(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for (index, ch) in text.chars().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push(ch.to_uppercase().next().unwrap_or(ch));
    }
    out
}

/// Where a click at column `dx` of an `EditAt` segment lands in `line`.
pub(crate) fn edit_offset(line: &str, byte_start: usize, dx: usize, spaced_heading: bool) -> usize {
    let start = floor_boundary(line, byte_start);
    let rest = &line[start..];
    if !spaced_heading {
        return start + byte_at_column(rest, dx);
    }
    // A spaced heading draws each char of the title followed by a space:
    // the clicked char of the spaced text, halved and rounded.
    let shown = spaced(rest.trim_end());
    let chars_in = shown[..byte_at_column(&shown, dx)].chars().count();
    let nth = chars_in.div_ceil(2);
    start
        + rest
            .char_indices()
            .nth(nth)
            .map_or(rest.len(), |(index, _)| index)
}

/// `HH:MM` for a Unix time shifted by `utc_offset` seconds.
pub(crate) fn hhmm(unix: u64, utc_offset: i64) -> String {
    let shifted = i64::try_from(unix).unwrap_or(0).saturating_add(utc_offset);
    let minutes = shifted.div_euclid(60);
    let day_minutes = minutes.rem_euclid(24 * 60);
    format!("{:02}:{:02}", day_minutes / 60, day_minutes % 60)
}

// ------------------------------------------------------------------ chrome

/// `── ✦ Label ✦ ────` with the label as a segment of its own.
fn rule_with(label: &str, width: usize, label_style: Style, target: InfoDockTarget) -> Vec<Seg> {
    // A label wider than the room left is cut with `…` (a prefix of the
    // source, so clicks still map).
    let room = width.saturating_sub(8).max(2);
    let shown = if label.width() > room {
        format!("{}…", &label[..cut_at_width(label, room - 1)])
    } else {
        label.to_owned()
    };
    let mut segs = vec![
        seg("──", fg(DUSK.rule)),
        seg(" ✦ ", fg(DUSK.gold)),
        hit(shown, label_style, target),
        seg(" ✦ ", fg(DUSK.gold)),
    ];
    let used = segs_width(&segs);
    segs.push(seg("─".repeat(width.saturating_sub(used)), fg(DUSK.rule)));
    segs
}

/// Row 0: `── ✦ Notes ✦ ─────`.
pub(crate) fn tab_strip(width: usize) -> Row {
    let mut segs = vec![
        seg("──", fg(DUSK.rule)),
        seg(" ✦ ", fg(DUSK.gold)),
        seg("Notes", bold(fg(DUSK.gold))),
        seg(" ✦ ", fg(DUSK.gold)),
    ];
    let used = segs_width(&segs);
    segs.push(seg("─".repeat(width.saturating_sub(used)), fg(DUSK.rule)));
    Row::new(segs, None)
}

/// Fork (session header): the tabs layout's title bar, `▤ Notes` left and
/// `✎  ✕` flush right (`✎` lit while editing).
pub(crate) fn title_bar(width: usize, editing: bool) -> Row {
    let mut segs = vec![
        seg(super::session_header::NOTES_DOCK_GLYPH, fg(DUSK.accent)),
        seg(" Notes", bold(fg(DUSK.ink))),
    ];
    let actions = [
        (
            "\u{270E}", // ✎
            if editing { DUSK.accent } else { DUSK.body },
            InfoDockTarget::Edit,
        ),
        ("\u{2715}", DUSK.body, InfoDockTarget::Close), // ✕
    ];
    // `✎  ✕` and a one-cell margin.
    let used = segs_width(&segs) + 1 + 2 + 1 + 1;
    if width > used {
        segs.push(seg(" ".repeat(width - used), fg(DUSK.body)));
        let mut first = true;
        for (glyph, color, target) in actions {
            if !first {
                segs.push(seg("  ", fg(DUSK.body)));
            }
            first = false;
            segs.push(hit(glyph, fg(color), target));
        }
    }
    Row::new(segs, None)
}

/// What the footer line reports, in priority order.
#[derive(Debug, Clone, Default)]
pub(crate) struct FooterInput<'a> {
    pub(crate) editing: bool,
    pub(crate) dragging: bool,
    pub(crate) flash: Option<&'a str>,
    /// A save conflicted: how many of their lines keep-mine re-adds.
    pub(crate) conflict: Option<usize>,
    /// The latest frame is wider than this client's pane surface.
    pub(crate) sized_elsewhere: bool,
}

pub(crate) fn footer(input: &FooterInput<'_>, width: usize) -> Row {
    if let Some(theirs) = input.conflict {
        return Row::new(
            vec![
                seg("› ", fg(DUSK.rust)),
                seg("changed elsewhere · ", italic(fg(DUSK.note))),
                hit("reload", fg(DUSK.gold), InfoDockTarget::ConflictReload),
                seg(" · ", fg(DUSK.dim)),
                hit(
                    format!("keep mine (+{theirs} theirs)"),
                    fg(DUSK.gold),
                    InfoDockTarget::ConflictKeepMine,
                ),
            ],
            None,
        );
    }
    if let Some(flash) = input.flash {
        return Row::new(
            vec![seg("› ", fg(DUSK.rust)), seg(flash, italic(fg(DUSK.note)))],
            None,
        );
    }
    if input.dragging {
        return Row::new(
            vec![
                seg("⇔ ", fg(DUSK.gold)),
                seg("release to keep this width", italic(fg(DUSK.note))),
            ],
            None,
        );
    }
    if input.sized_elsewhere {
        return Row::new(
            vec![
                seg("⇔ ", fg(DUSK.dim)),
                seg("sized by another client", italic(fg(DUSK.note))),
            ],
            None,
        );
    }
    let hints: &[(&str, &str)] = if input.editing {
        &[("esc", "save"), ("arrows", "move"), ("click", "cursor")]
    } else {
        &[
            ("click", "edit"),
            ("○", "tick"),
            ("wheel", "scroll"),
            ("e", "edit"),
            ("</>", "width"),
            ("esc", "pane"),
        ]
    };
    let mut segs = Vec::new();
    for (key, what) in hints {
        let piece = [
            seg(*key, fg(DUSK.gold)),
            seg(format!(" {what}  "), fg(DUSK.dim)),
        ];
        if segs_width(&segs) + segs_width(&piece) > width {
            break;
        }
        segs.extend(piece);
    }
    Row::new(segs, None)
}

/// A body line in place of content: loading, an error, an unsupported
/// server.
pub(crate) fn message_rows(text: &str, retry: bool, width: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = wrap_plain(text, width.saturating_sub(2))
        .into_iter()
        .map(|line| Row::new(vec![seg(format!("  {line}"), italic(fg(DUSK.dim)))], None))
        .collect();
    if retry {
        rows.push(Row::new(
            vec![
                seg("  ", fg(DUSK.dim)),
                hit("retry", fg(DUSK.gold), InfoDockTarget::Retry),
            ],
            None,
        ));
    }
    rows
}

// ------------------------------------------------------------------ notes

pub(crate) struct NotesInput<'a> {
    /// Already cleaned.
    pub(crate) text: &'a str,
    pub(crate) exists: bool,
    /// The focused tab's label: a `# heading` equal to it is not repeated.
    pub(crate) tab_label: &'a str,
    pub(crate) revision: &'a str,
    pub(crate) updated_by: Option<NotesAuthor>,
    pub(crate) updated_at: Option<u64>,
    pub(crate) utc_offset: i64,
    pub(crate) width: usize,
}

/// One inline run: `line[s..e]` drawn in `style`. The shown text of every
/// inline element (`code`, `**bold**`, `*italic*`, a link's text) is a
/// contiguous slice of the source, so a click maps straight back to it.
#[derive(Debug, Clone, Copy)]
struct Run {
    s: usize,
    e: usize,
    style: Style,
}

/// The first `close` in `line[from..end]`, as an absolute offset.
fn find_in(line: &str, from: usize, end: usize, close: &str) -> Option<usize> {
    line.get(from..end)?.find(close).map(|at| from + at)
}

/// The inline runs of `line[start..end]`: `` `code` `` gold, `**bold**` bold
/// ink, `*italic*` italic, `[text](url)` the text in accent, underlined
/// (the url is hidden). Delimiters are not drawn. Unclosed markers stay
/// literal text.
fn inline_runs(line: &str, start: usize, end: usize, base: Style) -> Vec<Run> {
    let bytes = line.as_bytes();
    let mut runs = Vec::new();
    let mut plain = start;
    let mut at = start;
    let flush = |runs: &mut Vec<Run>, s: usize, e: usize| {
        if e > s {
            runs.push(Run { s, e, style: base });
        }
    };
    while at < end {
        let token: Option<(usize, usize, usize, Style)> = match bytes[at] {
            b'`' => find_in(line, at + 1, end, "`")
                .filter(|close| *close > at + 1)
                .map(|close| (at + 1, close, close + 1, fg_on(DUSK.gold, base.bg))),
            b'*' if bytes.get(at + 1) == Some(&b'*') => find_in(line, at + 2, end, "**")
                .filter(|close| *close > at + 2)
                .map(|close| (at + 2, close, close + 2, bold(fg_on(DUSK.ink, base.bg)))),
            b'*' => {
                let opens = bytes
                    .get(at + 1)
                    .is_some_and(|next| !next.is_ascii_whitespace() && *next != b'*');
                opens
                    .then(|| find_in(line, at + 1, end, "*"))
                    .flatten()
                    .filter(|close| !bytes[close - 1].is_ascii_whitespace())
                    .map(|close| (at + 1, close, close + 1, italic(base)))
            }
            // The first `]` closes the text; a link needs `(` right after.
            b'[' => find_in(line, at + 1, end, "]")
                .filter(|mid| *mid > at + 1 && bytes.get(mid + 1) == Some(&b'('))
                .and_then(|mid| {
                    find_in(line, mid + 2, end, ")").map(|close| {
                        (
                            at + 1,
                            mid,
                            close + 1,
                            fg_on(DUSK.accent, base.bg).add_modifier(Modifier::UNDERLINED),
                        )
                    })
                }),
            _ => None,
        };
        match token {
            Some((s, e, next, style)) => {
                flush(&mut runs, plain, at);
                runs.push(Run { s, e, style });
                at = next;
                plain = next;
            }
            None => {
                at += line[at..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    flush(&mut runs, plain, end);
    runs
}

/// One visible char of the runs: its source span and run.
#[derive(Debug, Clone, Copy)]
struct Cell {
    s: usize,
    e: usize,
    width: usize,
    run: usize,
    space: bool,
}

fn run_cells(line: &str, runs: &[Run]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        for (offset, ch) in line[run.s..run.e].char_indices() {
            let s = run.s + offset;
            cells.push(Cell {
                s,
                e: s + ch.len_utf8(),
                width: char_width(ch),
                run: index,
                space: ch.is_whitespace(),
            });
        }
    }
    cells
}

/// Word-wrap cells to `first` columns on the first row and `rest` after:
/// ranges of cell indexes, one per row (whitespace between rows dropped, a
/// too-long word cut).
fn wrap_cells(cells: &[Cell], first: usize, rest: usize) -> Vec<(usize, usize)> {
    let (first, rest) = (first.max(1), rest.max(1));
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut begin: Option<usize> = None;
    for (index, cell) in cells.iter().enumerate() {
        match (cell.space, begin) {
            (true, Some(b)) => {
                words.push((b, index));
                begin = None;
            }
            (false, None) => begin = Some(index),
            _ => {}
        }
    }
    if let Some(b) = begin {
        words.push((b, cells.len()));
    }
    // Prefix sums: a span's width is O(1), so a long word stays linear.
    let mut prefix = Vec::with_capacity(cells.len() + 1);
    prefix.push(0usize);
    for cell in cells {
        prefix.push(prefix[prefix.len() - 1] + cell.width);
    }
    let span_width = |a: usize, b: usize| prefix[b] - prefix[a];
    let mut rows: Vec<(usize, usize)> = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    let room = |rows: &Vec<(usize, usize)>| if rows.is_empty() { first } else { rest };
    for (ws, we) in words {
        current = Some(match current {
            None => (ws, we),
            Some((s, _)) if span_width(s, we) <= room(&rows) => (s, we),
            Some(done) => {
                rows.push(done);
                (ws, we)
            }
        });
        while let Some((s, e)) = current {
            let width = room(&rows);
            if span_width(s, e) <= width {
                break;
            }
            let mut used = 0;
            let mut cut = s;
            while cut < e && used + cells[cut].width <= width {
                used += cells[cut].width;
                cut += 1;
            }
            let cut = cut.max(s + 1);
            rows.push((s, cut));
            current = Some((cut, e));
        }
    }
    rows.extend(current);
    rows
}

/// The segments of cells `a..b`: one per run piece, each editing at its
/// own source offset.
fn cell_segs(line: &str, li: usize, cells: &[Cell], runs: &[Run]) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let mut index = 0;
    while index < cells.len() {
        let first = cells[index];
        let mut last = index;
        while last + 1 < cells.len()
            && cells[last + 1].run == first.run
            && cells[last + 1].s == cells[last].e
        {
            last += 1;
        }
        out.push(hit(
            &line[first.s..cells[last].e],
            runs[first.run].style,
            InfoDockTarget::EditAt {
                line: li,
                byte_start: first.s,
                spaced: false,
            },
        ));
        index = last + 1;
    }
    out
}

/// `line[start..]` with inline formatting, wrapped to `width`: each row's
/// segments and the source byte its end maps to (the last row: the line's
/// end, so closing markers are inside).
fn wrapped_inline(
    line: &str,
    li: usize,
    start: usize,
    width: usize,
    base: Style,
) -> Vec<(Vec<Seg>, usize)> {
    let start = floor_boundary(line, start);
    let runs = inline_runs(line, start, line.len(), base);
    let cells = run_cells(line, &runs);
    let ranges = wrap_cells(&cells, width, width);
    if ranges.is_empty() {
        return vec![(vec![seg("", base)], line.len())];
    }
    let count = ranges.len();
    ranges
        .into_iter()
        .enumerate()
        .map(|(index, (a, b))| {
            let end = if index + 1 == count {
                line.len()
            } else {
                cells[b - 1].e
            };
            (cell_segs(line, li, &cells[a..b], &runs), end)
        })
        .collect()
}

/// Rows for a line with a fixed first-row prefix and a hanging indent of
/// the same width (`continuation` drawn in its place).
fn prefixed_rows(
    line: &str,
    li: usize,
    text_at: usize,
    width: usize,
    base: Style,
    first: Vec<Seg>,
    continuation: Seg,
) -> Vec<Row> {
    let lead = segs_width(&first);
    wrapped_inline(line, li, text_at, width.saturating_sub(lead).max(1), base)
        .into_iter()
        .enumerate()
        .map(|(index, (segs, end))| {
            let mut row = if index == 0 {
                first.clone()
            } else {
                vec![continuation.clone()]
            };
            row.extend(segs);
            Row::new(
                row,
                Some(InfoDockTarget::EditEnd {
                    line: li,
                    byte: end,
                }),
            )
        })
        .collect()
}

/// Leading spaces of `line` (tabs were expanded by [`clean`]).
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// `- [ ] text` / `- [x] text`: (done, byte offset of the text).
fn parse_task(line: &str) -> Option<(bool, usize)> {
    let indent = line.len() - line.trim_start().len();
    let rest = line[indent..].strip_prefix("- [")?;
    let mark = rest.chars().next()?;
    if !matches!(mark, ' ' | 'x' | 'X') {
        return None;
    }
    rest[1..].strip_prefix("] ")?;
    Some((mark != ' ', indent + "- [".len() + 1 + "] ".len()))
}

/// `- text` or `* text`: the byte offset of the text.
fn parse_bullet(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    (rest.starts_with("- ") || rest.starts_with("* ")).then_some(indent + 2)
}

/// `1. text` / `12) text`: (offset of the marker, its end, offset of the
/// text).
fn parse_numbered(line: &str) -> Option<(usize, usize, usize)> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line.as_bytes()[indent..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    if !matches!(rest.get(digits), Some(b'.' | b')')) || rest.get(digits + 1) != Some(&b' ') {
        return None;
    }
    Some((indent, indent + digits + 1, indent + digits + 2))
}

/// `HH:MM ` at `at`.
fn parse_time(line: &str, at: usize) -> bool {
    let bytes = line.as_bytes();
    bytes.len() > at + 5
        && bytes[at].is_ascii_digit()
        && bytes[at + 1].is_ascii_digit()
        && bytes[at + 2] == b':'
        && bytes[at + 3].is_ascii_digit()
        && bytes[at + 4].is_ascii_digit()
        && bytes[at + 5] == b' '
}

/// `Keyword: text` (a capitalised ASCII word): the keyword's byte length.
fn parse_keyword(line: &str) -> Option<usize> {
    let colon = line.find(": ")?;
    let word = &line[..colon];
    let mut chars = word.chars();
    let first = chars.next()?;
    (first.is_ascii_uppercase() && word.len() > 1 && chars.all(|ch| ch.is_ascii_lowercase()))
        .then_some(colon)
}

/// `---` (three or more dashes, nothing else): a plain rule.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3 && line.bytes().all(|b| b == b'-')
}

/// A fence line (`` ``` `` with an optional language).
fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// A table line: starts with `|`.
fn is_table(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// Tick or untick the task on `line`; `None` when it is not a task.
pub(crate) fn toggle_task_line(text: &str, line: usize) -> Option<String> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let current = *lines.get(line)?;
    let (done, _) = parse_task(current)?;
    let indent = current.len() - current.trim_start().len();
    let mark_at = indent + "- [".len();
    let toggled = format!(
        "{}{}{}",
        &current[..mark_at],
        if done { ' ' } else { 'x' },
        &current[mark_at + 1..]
    );
    lines[line] = &toggled;
    Some(lines.join("\n"))
}

/// A code block line: indented 2, a rule `▎`, the code in gold, all on the
/// selection background to the full width; wrapped with a 2-column hanging
/// indent. The source's own indentation is kept.
fn code_rows(line: &str, li: usize, width: usize) -> Vec<Row> {
    let on = |color: Color| fg_on(color, Some(DUSK.sel));
    let lead = indent_of(line);
    let inner = width.saturating_sub(4).max(1);
    let runs = [Run {
        s: lead,
        e: line.len(),
        style: on(DUSK.gold),
    }];
    let cells = run_cells(line, &runs);
    let lead = lead.min(inner / 2);
    let mut ranges = wrap_cells(&cells, inner.saturating_sub(lead), inner.saturating_sub(2));
    if ranges.is_empty() {
        ranges.push((0, 0));
    }
    let count = ranges.len();
    ranges
        .into_iter()
        .enumerate()
        .map(|(index, (a, b))| {
            let pad = if index == 0 {
                " ".repeat(lead)
            } else {
                "  ".to_owned()
            };
            let mut segs = vec![
                seg("  ", fg(DUSK.page)),
                seg("▎", on(DUSK.rule)),
                seg(" ", on(DUSK.gold)),
                seg(pad, on(DUSK.gold)),
            ];
            segs.extend(cell_segs(line, li, &cells[a..b], &runs));
            let used = segs_width(&segs);
            segs.push(seg(" ".repeat(width.saturating_sub(used)), on(DUSK.gold)));
            let end = if index + 1 == count || b == 0 {
                line.len()
            } else {
                cells[b - 1].e
            };
            Row::new(
                segs,
                Some(InfoDockTarget::EditEnd {
                    line: li,
                    byte: end,
                }),
            )
        })
        .collect()
}

/// A table cell: its source span (trimmed) in `line`.
fn table_cells(line: &str) -> Vec<(usize, usize)> {
    let trimmed_start = line.len() - line.trim_start().len();
    let mut start = trimmed_start;
    if line[start..].starts_with('|') {
        start += 1;
    }
    let mut end = line.trim_end().len();
    if end > start && line[..end].ends_with('|') {
        end -= 1;
    }
    let mut cells = Vec::new();
    let mut cell_start = start;
    let mut in_code = false;
    for (offset, ch) in line[start..end].char_indices() {
        match ch {
            '`' => in_code = !in_code,
            '|' if !in_code => {
                cells.push((cell_start, start + offset));
                cell_start = start + offset + 1;
            }
            _ => {}
        }
    }
    cells.push((cell_start, end));
    cells
        .into_iter()
        .map(|(s, e)| {
            let text = &line[s..e];
            let s2 = s + (text.len() - text.trim_start().len());
            (s2, s2.max(s + text.trim_end().len()))
        })
        .collect()
}

/// `|---|:--:|` and the like.
fn is_separator_row(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|(s, e)| {
            let cell = line[*s..*e].trim_matches(':');
            !cell.is_empty() && cell.bytes().all(|b| b == b'-')
        })
}

/// Narrow `widths` until they and their ` │ ` separators fit `budget`: the
/// widest columns give way first, all tied ones together, so the loop runs
/// once per distinct width (never once per column of excess).
fn shrink_columns(widths: &mut [usize], budget: usize) {
    let total =
        |widths: &[usize]| widths.iter().sum::<usize>() + 3 * widths.len().saturating_sub(1);
    loop {
        let over = total(widths).saturating_sub(budget);
        let widest = widths.iter().copied().max().unwrap_or(0);
        if over == 0 || widest <= 1 {
            return;
        }
        let tied = widths.iter().filter(|w| **w == widest).count();
        let next = widths
            .iter()
            .copied()
            .filter(|w| *w < widest)
            .max()
            .unwrap_or(1)
            .max(1);
        let target = widest.saturating_sub(over.div_ceil(tied)).max(next);
        // Ties all drop to `target`; lower ones keep their width.
        let mut left = over;
        for w in widths.iter_mut().filter(|w| **w == widest) {
            let cut = (widest - target).min(left);
            *w -= cut;
            left -= cut;
        }
    }
}

/// A table: aligned columns, `│` / `┼` separators in the rule colour, the
/// header (the row above a `|---|` row) in bold ink; wider than `width`,
/// the widest columns give way and their cells end in `…`.
fn table_rows(lines: &[(usize, &str)], width: usize) -> Vec<Row> {
    let header = lines.len() > 1 && is_separator_row(lines[1].1);
    struct Parsed<'a> {
        li: usize,
        line: &'a str,
        cells: Vec<(Vec<Run>, Vec<Cell>, usize)>,
        separator: bool,
    }
    let parsed: Vec<Parsed<'_>> = lines
        .iter()
        .enumerate()
        .map(|(row, &(li, line))| {
            let separator = header && row == 1;
            let base = if header && row == 0 {
                bold(fg(DUSK.ink))
            } else {
                fg(DUSK.body)
            };
            let cells = if separator {
                Vec::new()
            } else {
                table_cells(line)
                    .into_iter()
                    .map(|(s, e)| {
                        let runs = inline_runs(line, s, e, base);
                        let cells = run_cells(line, &runs);
                        let w = cells.iter().map(|c| c.width).sum();
                        (runs, cells, w)
                    })
                    .collect()
            };
            Parsed {
                li,
                line,
                cells,
                separator,
            }
        })
        .collect();
    let all_columns = parsed.iter().map(|p| p.cells.len()).max().unwrap_or(0);
    if all_columns == 0 {
        return Vec::new();
    }
    // Each column needs a cell and ` │ `: at most (width + 3) / 4 fit. The
    // ones past that are dropped behind a trailing ` │ …` (one more slot).
    let fit = width.div_ceil(4);
    let dropped = all_columns > fit.max(1);
    let columns = if dropped {
        fit.saturating_sub(1).max(1)
    } else {
        all_columns
    };
    let budget = if dropped {
        width.saturating_sub(4)
    } else {
        width
    };
    let mut widths = vec![0usize; columns];
    for p in &parsed {
        for (index, cell) in p.cells.iter().take(columns).enumerate() {
            widths[index] = widths[index].max(cell.2);
        }
    }
    shrink_columns(&mut widths, budget);
    // Pad the last kept column when the ` │ …` follows it.
    let padded = |index: usize| index + 1 < columns || dropped;
    let rule = fg(DUSK.rule);
    parsed
        .iter()
        .map(|p| {
            let target = Some(InfoDockTarget::EditEnd {
                line: p.li,
                byte: p.line.len(),
            });
            if p.separator {
                let mut text = String::new();
                for (index, w) in widths.iter().enumerate() {
                    if index > 0 {
                        text.push('┼');
                    }
                    let lead = usize::from(index > 0);
                    let trail = usize::from(padded(index));
                    text.push_str(&"─".repeat(w + lead + trail));
                }
                if dropped {
                    text.push_str("┼──");
                }
                return Row::new(vec![seg(text, rule)], target);
            }
            let mut segs = Vec::new();
            for (index, w) in widths.iter().enumerate() {
                if index > 0 {
                    segs.push(seg(" │ ", rule));
                }
                let Some((runs, cells, cell_w)) = p.cells.get(index) else {
                    if padded(index) {
                        segs.push(seg(" ".repeat(*w), fg(DUSK.body)));
                    }
                    continue;
                };
                let (shown, cut) = if cell_w <= w {
                    (cells.len(), false)
                } else {
                    let mut used = 0;
                    let mut take = 0;
                    while take < cells.len() && used + cells[take].width < *w {
                        used += cells[take].width;
                        take += 1;
                    }
                    (take, true)
                };
                segs.extend(cell_segs(p.line, p.li, &cells[..shown], runs));
                let mut drawn: usize = cells[..shown].iter().map(|c| c.width).sum();
                if cut {
                    segs.push(seg("…", fg(DUSK.dim)));
                    drawn += 1;
                }
                if padded(index) {
                    segs.push(seg(" ".repeat(w.saturating_sub(drawn)), fg(DUSK.body)));
                }
            }
            if dropped {
                segs.push(seg(" │ ", rule));
                segs.push(seg("…", fg(DUSK.dim)));
            }
            Row::new(segs, target)
        })
        .collect()
}

/// The Notes view's read rows (`notes_rows` in the stub), with the picked
/// markdown rules. Pure; the dock memoises the rows, so this runs only when
/// the notes, the width or the labels change.
pub(crate) fn notes_rows(input: &NotesInput<'_>) -> Vec<Row> {
    let width = input.width;
    let mut rows: Vec<Row> = Vec::new();
    if !input.exists && input.text.trim().is_empty() {
        rows.push(Row::new(
            vec![seg(
                "no notes yet · click here or press e to start",
                italic(fg(DUSK.dim)),
            )],
            Some(InfoDockTarget::EditEnd { line: 0, byte: 0 }),
        ));
        return rows;
    }
    let lines: Vec<&str> = input.text.split('\n').map(str::trim_end).collect();
    let mut in_fence = false;
    let mut li = 0;
    while li < lines.len() {
        let line = lines[li];
        let end = Some(InfoDockTarget::EditEnd {
            line: li,
            byte: line.len(),
        });
        if in_fence {
            if is_fence(line) {
                in_fence = false;
            } else {
                rows.extend(code_rows(line, li, width));
            }
            li += 1;
            continue;
        }
        if is_fence(line) {
            in_fence = true;
            li += 1;
            continue;
        }
        if line.is_empty() {
            rows.push(Row::new(
                Vec::new(),
                Some(InfoDockTarget::EditEnd { line: li, byte: 0 }),
            ));
            li += 1;
            continue;
        }
        if is_table(line) {
            let first = li;
            while li < lines.len() && is_table(lines[li]) {
                li += 1;
            }
            let block: Vec<(usize, &str)> =
                (first..li).map(|index| (index, lines[index])).collect();
            rows.extend(table_rows(&block, width));
            continue;
        }
        li += 1;
        let li = li - 1;
        if let Some(title) = line.strip_prefix("# ") {
            let title = title.trim();
            if title == input.tab_label.trim() {
                continue;
            }
            let at = line.len() - line[2..].trim_start().len();
            let shown = spaced(title);
            if shown.width() + 2 > width {
                // Too wide spaced: the plain title, wrapped under itself.
                rows.extend(prefixed_rows(
                    line,
                    li,
                    at,
                    width,
                    bold(fg(DUSK.ink)),
                    vec![hit(
                        "❦ ",
                        fg(DUSK.gold),
                        InfoDockTarget::EditEnd { line: li, byte: 0 },
                    )],
                    seg("  ", fg(DUSK.ink)),
                ));
                continue;
            }
            rows.push(Row::new(
                vec![
                    hit(
                        "❦ ",
                        fg(DUSK.gold),
                        InfoDockTarget::EditEnd { line: li, byte: 0 },
                    ),
                    hit(
                        shown,
                        bold(fg(DUSK.ink)),
                        InfoDockTarget::EditAt {
                            line: li,
                            byte_start: at,
                            spaced: true,
                        },
                    ),
                ],
                end,
            ));
            continue;
        }
        if let Some(label) = line.strip_prefix("## ") {
            let label = label.trim();
            let at = line.len() - line[3..].trim_start().len();
            rows.push(Row::new(
                rule_with(
                    label,
                    width,
                    fg(DUSK.gold),
                    InfoDockTarget::EditAt {
                        line: li,
                        byte_start: at,
                        spaced: false,
                    },
                ),
                end.clone(),
            ));
            // A section rule always has air below it.
            if lines.get(li + 1).is_some_and(|next| !next.is_empty()) {
                rows.push(Row::new(Vec::new(), end));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            let at = line.len() - rest.trim_start().len();
            rows.extend(prefixed_rows(
                line,
                li,
                at,
                width,
                bold(fg(DUSK.gold)),
                Vec::new(),
                seg("", fg(DUSK.gold)),
            ));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#### ") {
            let at = line.len() - rest.trim_start().len();
            rows.extend(prefixed_rows(
                line,
                li,
                at,
                width,
                italic(fg(DUSK.gold)),
                vec![seg("  ", fg(DUSK.gold))],
                seg("  ", fg(DUSK.gold)),
            ));
            continue;
        }
        if is_rule(line) {
            rows.push(Row::new(vec![seg("─".repeat(width), fg(DUSK.rule))], end));
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix('>') {
            let at = line.len() - rest.trim_start().len();
            rows.extend(prefixed_rows(
                line,
                li,
                at,
                width,
                italic(fg(DUSK.body)),
                vec![seg("  ", fg(DUSK.rule)), seg("▎ ", fg(DUSK.rule))],
                seg("    ", fg(DUSK.rule)),
            ));
            continue;
        }
        let indent = indent_of(line).min(width / 2);
        let pad = " ".repeat(indent);
        if let Some((done, text_at)) = parse_task(line) {
            let mark = hit(
                if done { "● " } else { "○ " },
                fg(if done { DUSK.gold } else { DUSK.dim }),
                InfoDockTarget::Task {
                    line: li,
                    revision: input.revision.to_owned(),
                },
            );
            let style = fg(if done { DUSK.dim } else { DUSK.body });
            rows.extend(prefixed_rows(
                line,
                li,
                text_at,
                width,
                style,
                vec![seg(pad.clone(), style), mark],
                seg(format!("{pad}  "), style),
            ));
            continue;
        }
        if let Some(text_at) = parse_bullet(line) {
            let timed = parse_time(line, text_at);
            let first = if timed {
                vec![
                    seg(pad.clone(), fg(DUSK.dim)),
                    hit(
                        &line[text_at..text_at + 5],
                        fg(DUSK.dim),
                        InfoDockTarget::EditAt {
                            line: li,
                            byte_start: text_at,
                            spaced: false,
                        },
                    ),
                    hit(
                        " · ",
                        fg(DUSK.rust),
                        InfoDockTarget::EditEnd {
                            line: li,
                            byte: text_at + 6,
                        },
                    ),
                ]
            } else {
                vec![
                    seg(pad.clone(), fg(DUSK.rust)),
                    hit(
                        "· ",
                        fg(DUSK.rust),
                        InfoDockTarget::EditEnd {
                            line: li,
                            byte: text_at,
                        },
                    ),
                ]
            };
            let lead = segs_width(&first);
            rows.extend(prefixed_rows(
                line,
                li,
                if timed { text_at + 6 } else { text_at },
                width,
                fg(DUSK.body),
                first,
                seg(" ".repeat(lead), fg(DUSK.body)),
            ));
            continue;
        }
        if let Some((marker_at, marker_end, text_at)) = parse_numbered(line) {
            let first = vec![
                seg(pad.clone(), fg(DUSK.rust)),
                hit(
                    &line[marker_at..marker_end],
                    fg(DUSK.rust),
                    InfoDockTarget::EditEnd {
                        line: li,
                        byte: text_at,
                    },
                ),
                seg(" ", fg(DUSK.rust)),
            ];
            let lead = segs_width(&first);
            rows.extend(prefixed_rows(
                line,
                li,
                text_at,
                width,
                fg(DUSK.body),
                first,
                seg(" ".repeat(lead), fg(DUSK.body)),
            ));
            continue;
        }
        if let Some(keyword_len) = parse_keyword(line) {
            let lead = keyword_len + 2;
            let lead_width = line[..lead].width();
            rows.extend(prefixed_rows(
                line,
                li,
                lead,
                width,
                fg(DUSK.body),
                vec![hit(
                    &line[..lead],
                    italic(fg(DUSK.note)),
                    InfoDockTarget::EditAt {
                        line: li,
                        byte_start: 0,
                        spaced: false,
                    },
                )],
                seg(" ".repeat(lead_width), fg(DUSK.note)),
            ));
            continue;
        }
        rows.extend(prefixed_rows(
            line,
            li,
            0,
            width,
            fg(DUSK.body),
            Vec::new(),
            seg("", fg(DUSK.body)),
        ));
    }
    while rows.first().is_some_and(|row| row.segs.is_empty()) {
        rows.remove(0);
    }
    rows.push(Row::default());
    let short = input.revision.rsplit(':').next().unwrap_or(input.revision);
    let short: String = short.chars().take(8).collect();
    let mut meta = format!("rev {short}");
    match input.updated_by {
        Some(NotesAuthor::User) => meta.push_str(" · you"),
        Some(NotesAuthor::Agent) => meta.push_str(" · agent"),
        Some(NotesAuthor::Unknown) | None => {}
    }
    if let Some(at) = input.updated_at {
        meta.push_str(" · ");
        meta.push_str(&hhmm(at, input.utc_offset));
    }
    rows.push(Row::new(
        vec![seg("❧ ", fg(DUSK.gold)), seg(meta, italic(fg(DUSK.dim)))],
        None,
    ));
    rows
}

/// The editor's rows: each buffer line hard-wrapped at `width` columns, the
/// cursor cell drawn reversed. Returns the rows and the cursor's row.
pub(crate) fn editor_rows(
    lines: &[String],
    cursor: (usize, usize),
    width: usize,
) -> (Vec<Row>, usize) {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut cursor_row = 0;
    let cursor_style = Style::default().fg(DUSK.page).bg(DUSK.ink);
    for (index, line) in lines.iter().enumerate() {
        let style = if line.starts_with('#') {
            bold(fg(DUSK.ink))
        } else {
            fg(DUSK.body)
        };
        // Chunks of at most `width` columns; a cursor at the very end of a
        // full chunk gets an empty chunk of its own.
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < line.len() {
            let cut = start + cut_at_width(&line[start..], width);
            chunks.push((start, cut));
            start = cut;
        }
        if chunks.is_empty()
            || (cursor.0 == index
                && line[chunks[chunks.len() - 1].0..].width() >= width
                && cursor.1 >= line.len())
        {
            chunks.push((line.len(), line.len()));
        }
        let at = |byte_start: usize| InfoDockTarget::EditAt {
            line: index,
            byte_start,
            spaced: false,
        };
        let last = chunks.len() - 1;
        for (chunk_index, (s, e)) in chunks.into_iter().enumerate() {
            let has_cursor =
                cursor.0 == index && cursor.1 >= s && (cursor.1 < e || chunk_index == last);
            let mut segs = Vec::new();
            if has_cursor {
                let c = floor_boundary(line, cursor.1.min(e));
                if c > s {
                    segs.push(hit(&line[s..c], style, at(s)));
                }
                let next = line[c..e].chars().next();
                match next {
                    Some(ch) => {
                        let after = c + ch.len_utf8();
                        segs.push(hit(&line[c..after], cursor_style, at(c)));
                        if after < e {
                            segs.push(hit(&line[after..e], style, at(after)));
                        }
                    }
                    None => segs.push(hit(" ", cursor_style, at(c))),
                }
                cursor_row = rows.len();
            } else if e > s {
                segs.push(hit(&line[s..e], style, at(s)));
            }
            rows.push(Row::new(
                segs,
                Some(InfoDockTarget::EditEnd {
                    line: index,
                    byte: e,
                }),
            ));
        }
    }
    (rows, cursor_row)
}

// ------------------------------------------------------------------ merge

/// Keep mine after a conflict: the user's buffer, plus the lines their
/// version added (absent from both the common base and the buffer), each
/// placed after the nearest preceding non-blank line of theirs the result
/// already has, else at the end. Lines they removed stay (the buffer wins).
/// Returns the merged lines and how many lines were re-added.
pub(crate) fn keep_mine_merge(base: &str, mine: &[String], theirs: &str) -> (Vec<String>, usize) {
    let base_lines: HashSet<&str> = base.split('\n').collect();
    let mine_lines: HashSet<&str> = mine.iter().map(String::as_str).collect();
    let theirs_lines: Vec<&str> = theirs.split('\n').collect();
    let mut merged = mine.to_vec();
    let mut added = 0;
    for (index, line) in theirs_lines.iter().enumerate() {
        if base_lines.contains(line) || mine_lines.contains(line) {
            continue;
        }
        let anchor = theirs_lines[..index]
            .iter()
            .rev()
            .filter(|prev| !prev.trim().is_empty())
            .find_map(|prev| merged.iter().rposition(|have| have == prev));
        match anchor {
            Some(position) => merged.insert(position + 1, (*line).to_owned()),
            None => merged.push((*line).to_owned()),
        }
        added += 1;
    }
    (merged, added)
}

/// How many lines [`keep_mine_merge`] would re-add.
pub(crate) fn theirs_added(base: &str, mine: &[String], theirs: &str) -> usize {
    let base_lines: HashSet<&str> = base.split('\n').collect();
    let mine_lines: HashSet<&str> = mine.iter().map(String::as_str).collect();
    theirs
        .split('\n')
        .filter(|line| !base_lines.contains(line) && !mine_lines.contains(line))
        .count()
}
