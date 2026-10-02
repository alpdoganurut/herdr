//! The info dock's row model (fork): pure functions that turn notes text,
//! checkpoints and transcript context into drawable rows, ported from the
//! picked design stub (`c-almanac.py`: `history_rows`, `context_rows`,
//! `notes_rows`, `editor_rows`, the tab strip and the footer).
//!
//! Rows are plain data (`Row` of `Seg`s with optional click targets); the
//! dock state memoises them and `info_dock_render` only draws them. Every
//! byte offset a target carries is a char boundary of the line it names.

use std::collections::HashSet;

use ratatui::style::{Color, Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::api::schema::notes::{
    CheckpointContextInfo, CheckpointContextSource, CheckpointInfo, CheckpointKind, NotesAuthor,
};

/// The dock's two views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum InfoView {
    Notes,
    #[default]
    History,
}

/// What a click in the dock does; the last registered target under the
/// pointer wins (a segment beats its row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InfoDockTarget {
    View(InfoView),
    /// A kind chip (`None` is "all"); clicking the lit chip clears it.
    Filter(Option<CheckpointKind>),
    /// Expand every visible checkpoint, or collapse all when all are open.
    ExpandAll,
    /// A checkpoint's title row: select, or toggle when already selected.
    Row(String),
    /// A checkpoint's time, glyph or caret: select and toggle.
    Toggle(String),
    /// Inside an open checkpoint: select only.
    Body(String),
    /// Long context: show more / show less.
    ContextMore(String),
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

/// Like [`wrap_plain`], but as byte spans of `line` (from `start`), so a
/// click maps back to an offset in the source line.
pub(crate) fn wrap_spans(line: &str, width: usize, start: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    let start = floor_boundary(line, start);
    for (word_start, word_end) in words(line, start) {
        current = Some(match current {
            None => (word_start, word_end),
            Some((s, _)) if line[s..word_end].width() <= width => (s, word_end),
            Some(done) => {
                spans.push(done);
                (word_start, word_end)
            }
        });
        while let Some((s, e)) = current {
            if width == 0 || line[s..e].width() <= width {
                break;
            }
            let cut = s + cut_at_width(&line[s..e], width);
            spans.push((s, cut));
            current = Some((cut, e));
        }
    }
    match current {
        Some(span) => spans.push(span),
        None => spans.push((start, start)),
    }
    spans
}

/// Byte spans of the non-whitespace runs of `line` from `start`.
fn words(line: &str, start: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut word: Option<usize> = None;
    for (index, ch) in line[start..].char_indices() {
        let index = start + index;
        if ch.is_whitespace() {
            if let Some(begin) = word.take() {
                out.push((begin, index));
            }
        } else if word.is_none() {
            word = Some(index);
        }
    }
    if let Some(begin) = word {
        out.push((begin, line.len()));
    }
    out
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

// ------------------------------------------------------------------ kinds

/// Kinds in chip order.
pub(crate) const KIND_ORDER: [CheckpointKind; 5] = [
    CheckpointKind::Decision,
    CheckpointKind::Milestone,
    CheckpointKind::Failure,
    CheckpointKind::Bookmark,
    CheckpointKind::Note,
];

/// The filter cycle (`f`): all, then each kind.
pub(crate) const FILTERS: [Option<CheckpointKind>; 6] = [
    None,
    Some(CheckpointKind::Decision),
    Some(CheckpointKind::Milestone),
    Some(CheckpointKind::Failure),
    Some(CheckpointKind::Bookmark),
    Some(CheckpointKind::Note),
];

/// A kind's (agent glyph, user glyph, color): hollow for the agent's own
/// checkpoints, filled for the user's.
fn kind_marks(kind: CheckpointKind) -> (&'static str, &'static str, Color) {
    match kind {
        CheckpointKind::Decision => ("◇", "◆", DUSK.gold),
        CheckpointKind::Milestone => ("○", "●", DUSK.accent),
        CheckpointKind::Failure => ("×", "✖", DUSK.rust),
        CheckpointKind::Bookmark => ("⚐", "⚑", DUSK.ink),
        CheckpointKind::Note | CheckpointKind::Unknown => ("·", "•", DUSK.dim),
    }
}

fn node(checkpoint: &CheckpointInfo) -> (&'static str, Color) {
    let (agent, user, color) = kind_marks(checkpoint.kind);
    if checkpoint.author == NotesAuthor::User {
        (user, color)
    } else {
        (agent, color)
    }
}

// ------------------------------------------------------------------ chrome

/// `── ✦ Label ✦ ────` with the label as a segment of its own.
fn rule_with(label: &str, width: usize, label_style: Style, target: InfoDockTarget) -> Vec<Seg> {
    let mut segs = vec![
        seg("──", fg(DUSK.rule)),
        seg(" ✦ ", fg(DUSK.gold)),
        hit(label, label_style, target),
        seg(" ✦ ", fg(DUSK.gold)),
    ];
    let used = segs_width(&segs);
    segs.push(seg("─".repeat(width.saturating_sub(used)), fg(DUSK.rule)));
    segs
}

/// Row 0: `── ✦ Notes ✦ ──  History  ───`; each tab is a click target.
pub(crate) fn tab_strip(view: InfoView, width: usize) -> Row {
    let tab = |name: &str, target: InfoView| -> Vec<Seg> {
        let act = InfoDockTarget::View(target);
        if view == target {
            vec![
                hit(" ✦ ", fg(DUSK.gold), act.clone()),
                hit(name, bold(fg(DUSK.gold)), act.clone()),
                hit(" ✦ ", fg(DUSK.gold), act),
            ]
        } else {
            vec![
                hit("  ", fg(DUSK.rule), act.clone()),
                hit(name, fg(DUSK.dim), act.clone()),
                hit("  ", fg(DUSK.rule), act),
            ]
        }
    };
    let mut segs = vec![seg("──", fg(DUSK.rule))];
    segs.extend(tab("Notes", InfoView::Notes));
    segs.push(seg("──", fg(DUSK.rule)));
    segs.extend(tab("History", InfoView::History));
    let used = segs_width(&segs);
    segs.push(seg("─".repeat(width.saturating_sub(used)), fg(DUSK.rule)));
    Row::new(segs, None)
}

/// What the footer line reports, in priority order.
#[derive(Debug, Clone, Default)]
pub(crate) struct FooterInput<'a> {
    pub(crate) view: InfoView,
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
    let hints: &[(&str, &str)] = match (input.view, input.editing) {
        (_, true) => &[("esc", "save"), ("arrows", "move"), ("click", "cursor")],
        (InfoView::History, false) => &[
            ("click", "open"),
            ("wheel", "scroll"),
            ("j/k", "move"),
            ("␣", "expand"),
            ("e/E", "all"),
            ("f", "kind"),
            ("b", "pin"),
            ("</>", "width"),
            ("⇥", "notes"),
            ("esc", "pane"),
        ],
        (InfoView::Notes, false) => &[
            ("click", "edit"),
            ("○", "tick"),
            ("wheel", "scroll"),
            ("e", "edit"),
            ("</>", "width"),
            ("⇥", "history"),
            ("esc", "pane"),
        ],
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

// ------------------------------------------------------------------ history

/// Lines of context shown before "show more".
pub(crate) const CTX_LINES: usize = 6;

/// What the dock knows of a checkpoint's transcript context.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ContextView<'a> {
    /// The checkpoint has no anchor.
    None,
    /// Not fetched yet, or the server is still reading.
    Loading,
    Loaded(&'a CheckpointContextInfo),
}

pub(crate) struct HistoryInput<'a> {
    /// Oldest first.
    pub(crate) checkpoints: &'a [CheckpointInfo],
    pub(crate) filter: Option<CheckpointKind>,
    pub(crate) selected: Option<&'a str>,
    pub(crate) expanded: &'a HashSet<String>,
    pub(crate) ctx_full: &'a HashSet<String>,
    /// Who "by <agent>" and the reply head name.
    pub(crate) agent: &'a str,
    pub(crate) now: u64,
    pub(crate) utc_offset: i64,
    /// Content columns (the dock width less the edge and marker columns).
    pub(crate) width: usize,
}

pub(crate) struct HistoryRows {
    /// The chips row and the spine row with expand / collapse all; they do
    /// not scroll.
    pub(crate) pinned: Vec<Row>,
    pub(crate) rows: Vec<Row>,
    /// The selected checkpoint's first and last row.
    pub(crate) selected: Option<(usize, usize)>,
}

/// The checkpoints `filter` lets through, oldest first.
pub(crate) fn visible(
    checkpoints: &[CheckpointInfo],
    filter: Option<CheckpointKind>,
) -> impl Iterator<Item = &CheckpointInfo> {
    checkpoints
        .iter()
        .filter(move |checkpoint| filter.is_none_or(|kind| checkpoint.kind == kind))
}

pub(crate) fn all_expanded(
    checkpoints: &[CheckpointInfo],
    filter: Option<CheckpointKind>,
    expanded: &HashSet<String>,
) -> bool {
    let mut any = false;
    for checkpoint in visible(checkpoints, filter) {
        any = true;
        if !expanded.contains(&checkpoint.id) {
            return false;
        }
    }
    any
}

fn is_continuing(prompt: &str) -> bool {
    let inner = prompt
        .trim()
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .map(str::trim);
    inner.is_some_and(|word| word.to_lowercase().starts_with("continu"))
}

/// The you / agent exchange, capped at [`CTX_LINES`] text lines with a show
/// more / show less toggle.
fn context_rows<'a>(
    input: &HistoryInput<'_>,
    checkpoint: &CheckpointInfo,
    context: ContextView<'a>,
    spine: &[Seg],
) -> Vec<Row> {
    let id = &checkpoint.id;
    let body = || Some(InfoDockTarget::Body(id.clone()));
    let line = |segs: Vec<Seg>| {
        let mut all = spine.to_vec();
        all.extend(segs);
        Row::new(all, body())
    };
    let note = |text: &str| {
        line(vec![
            seg("┆ ", fg(DUSK.rule)),
            seg(text, italic(fg(DUSK.dim))),
        ])
    };
    let context = match context {
        ContextView::None => return vec![note("no transcript context")],
        ContextView::Loading => return vec![note("loading context…")],
        ContextView::Loaded(context) => context,
    };
    match context.source {
        CheckpointContextSource::Pending => return vec![note("loading context…")],
        CheckpointContextSource::Missing => return vec![note("transcript not found")],
        CheckpointContextSource::Unsupported | CheckpointContextSource::Unknown => {
            return vec![note("no transcript context")];
        }
        CheckpointContextSource::Native
        | CheckpointContextSource::Backup
        | CheckpointContextSource::BackupPrevious => {}
    }
    let text_width = input.width.saturating_sub(10).max(1);
    // (is_text, segs)
    let mut items: Vec<(bool, Vec<Seg>)> = Vec::new();
    let prompt = clean(context.prompt.as_deref().unwrap_or_default());
    let prompt = prompt.trim();
    let reply = clean(context.reply.as_deref().unwrap_or_default());
    let reply = reply.trim();
    let continued = (context.continued && prompt.is_empty()) || is_continuing(prompt);
    if continued || !prompt.is_empty() {
        items.push((
            false,
            vec![seg("┆ ", fg(DUSK.rule)), seg("you", fg(DUSK.gold))],
        ));
        let (text, style) = if continued {
            ("(continued from previous turn)", italic(fg(DUSK.dim)))
        } else {
            (prompt, italic(fg(DUSK.note)))
        };
        for wrapped in wrap_plain(text, text_width) {
            items.push((true, vec![seg("┆ ", fg(DUSK.rule)), seg(wrapped, style)]));
        }
    }
    if !reply.is_empty() {
        items.push((
            false,
            vec![seg("┆ ", fg(DUSK.rule)), seg(input.agent, fg(DUSK.gold))],
        ));
        for wrapped in wrap_plain(reply, text_width) {
            items.push((
                true,
                vec![seg("┆ ", fg(DUSK.rule)), seg(wrapped, fg(DUSK.body))],
            ));
        }
    }
    if items.is_empty() {
        return vec![note("no text near this checkpoint")];
    }
    let text_lines = items.iter().filter(|(text, _)| *text).count();
    let full = input.ctx_full.contains(id);
    if text_lines > CTX_LINES && !full {
        let mut kept = Vec::new();
        let mut shown = 0;
        for (text, segs) in items {
            if text {
                if shown == CTX_LINES {
                    break;
                }
                shown += 1;
            }
            kept.push((text, segs));
        }
        while kept.last().is_some_and(|(text, _)| !*text) {
            kept.pop();
        }
        items = kept;
    }
    let mut rows = vec![line(vec![seg("┆", fg(DUSK.rule))])];
    rows.extend(items.into_iter().map(|(_, segs)| line(segs)));
    if text_lines > CTX_LINES {
        let act = InfoDockTarget::ContextMore(id.clone());
        let toggle = if full {
            vec![hit("show less ▴", fg(DUSK.gold), act)]
        } else {
            vec![
                hit("show more ▾", fg(DUSK.gold), act.clone()),
                hit(
                    format!("  +{} lines", text_lines - CTX_LINES),
                    italic(fg(DUSK.dim)),
                    act,
                ),
            ]
        };
        let mut segs = vec![seg("┆ ", fg(DUSK.rule))];
        segs.extend(toggle);
        rows.push(line(segs));
    }
    if context.truncated && (full || text_lines <= CTX_LINES) {
        rows.push(line(vec![
            seg("┆ ", fg(DUSK.rule)),
            seg("(cut short)", italic(fg(DUSK.dim))),
        ]));
    }
    rows
}

/// The History view's rows (`history_rows` in the stub). `context` answers
/// what is known of a checkpoint's transcript context.
pub(crate) fn history_rows<'a>(
    input: &HistoryInput<'_>,
    context: impl Fn(&CheckpointInfo) -> ContextView<'a>,
) -> HistoryRows {
    let width = input.width;
    let all = input.checkpoints;
    let filter = input.filter;
    // Chips: a count per kind, the active filter lit, "all" clears.
    let mut head = Vec::new();
    for kind in KIND_ORDER {
        let count = all.iter().filter(|cp| cp.kind == kind).count();
        let on = filter == Some(kind);
        let bg = on.then_some(DUSK.sel);
        let (glyph, _, color) = kind_marks(kind);
        let act = InfoDockTarget::Filter(Some(kind));
        let glyph_style = fg_on(color, bg);
        head.push(hit(
            glyph,
            if on { bold(glyph_style) } else { glyph_style },
            act.clone(),
        ));
        head.push(hit(
            format!(" {count}"),
            fg_on(if on { DUSK.ink } else { DUSK.dim }, bg),
            act,
        ));
        head.push(seg("  ", fg(DUSK.dim)));
    }
    let all_chip = hit(
        "all",
        if filter.is_none() {
            bold(fg_on(DUSK.ink, Some(DUSK.sel)))
        } else {
            italic(fg(DUSK.note))
        },
        InfoDockTarget::Filter(None),
    );
    if segs_width(&head) + 3 < width {
        let pad = width - segs_width(&head) - 3;
        head.push(seg(" ".repeat(pad), fg(DUSK.dim)));
        head.push(all_chip);
    }
    let any_visible = visible(all, filter).next().is_some();
    let mut spine0 = vec![seg(format!("{}┊", " ".repeat(6)), fg(DUSK.rule))];
    if any_visible {
        let act = InfoDockTarget::ExpandAll;
        let toggle = if all_expanded(all, filter, input.expanded) {
            vec![
                hit("collapse all ", fg(DUSK.dim), act.clone()),
                hit("▴", fg(DUSK.gold), act),
            ]
        } else {
            vec![
                hit("expand all ", fg(DUSK.dim), act.clone()),
                hit("▾", fg(DUSK.gold), act),
            ]
        };
        if segs_width(&spine0) + segs_width(&toggle) < width {
            let pad = width - segs_width(&spine0) - segs_width(&toggle);
            spine0.push(seg(" ".repeat(pad), fg(DUSK.dim)));
            spine0.extend(toggle);
        }
    }
    let pinned = vec![Row::new(head, None), Row::new(spine0, None)];

    let mut rows = Vec::new();
    let mut selected = None;
    let title_width = width.saturating_sub(10).max(1);
    let spine = vec![
        seg(" ".repeat(6), fg(DUSK.dim)),
        seg("│", fg(DUSK.rule)),
        seg(" ", fg(DUSK.dim)),
    ];
    for checkpoint in visible(all, filter) {
        let id = &checkpoint.id;
        let (glyph, color) = node(checkpoint);
        let is_selected = input.selected == Some(id.as_str());
        let opened = input.expanded.contains(id);
        let bg = is_selected.then_some(DUSK.sel);
        let title_style = if is_selected {
            bold(fg_on(DUSK.ink, bg))
        } else {
            fg_on(if opened { DUSK.ink } else { DUSK.body }, bg)
        };
        let first = rows.len();
        let toggle = InfoDockTarget::Toggle(id.clone());
        for (index, line) in wrap_plain(&clean(&checkpoint.title), title_width)
            .into_iter()
            .enumerate()
        {
            let line_width = line.width();
            let mut segs = if index == 0 {
                let glyph_style = fg_on(color, bg);
                vec![
                    hit(
                        format!("{} ", hhmm(checkpoint.ts, input.utc_offset)),
                        fg_on(DUSK.dim, bg),
                        toggle.clone(),
                    ),
                    hit(
                        glyph,
                        if checkpoint.author == NotesAuthor::User {
                            bold(glyph_style)
                        } else {
                            glyph_style
                        },
                        toggle.clone(),
                    ),
                    seg(" ", fg_on(DUSK.dim, bg)),
                ]
            } else {
                vec![
                    seg(" ".repeat(6), fg_on(DUSK.dim, bg)),
                    seg("│", fg_on(DUSK.rule, bg)),
                    seg(" ", fg_on(DUSK.dim, bg)),
                ]
            };
            segs.push(seg(line, title_style));
            if index == 0 {
                let pad = width.saturating_sub(8 + line_width + 1).max(1);
                segs.push(seg(" ".repeat(pad), fg_on(DUSK.dim, bg)));
                segs.push(hit(
                    if opened { "▾" } else { "▸" },
                    fg_on(if opened { DUSK.gold } else { DUSK.dim }, bg),
                    toggle.clone(),
                ));
            }
            rows.push(Row::new(segs, Some(InfoDockTarget::Row(id.clone()))));
        }
        if opened {
            let body = || Some(InfoDockTarget::Body(id.clone()));
            let with_spine = |segs: Vec<Seg>| {
                let mut all = spine.clone();
                all.extend(segs);
                all
            };
            if let Some(detail) = checkpoint.detail.as_deref().filter(|d| !d.is_empty()) {
                for line in wrap_plain(&clean(detail), width.saturating_sub(8).max(1)) {
                    rows.push(Row::new(
                        with_spine(vec![seg(line, italic(fg(DUSK.note)))]),
                        body(),
                    ));
                }
            }
            let mut meta = if checkpoint.author == NotesAuthor::User {
                "by you".to_owned()
            } else {
                format!("by {}", input.agent)
            };
            if !checkpoint.tags.is_empty() {
                meta.push(' ');
                for tag in &checkpoint.tags {
                    meta.push_str(" #");
                    meta.push_str(&clean(tag));
                }
            }
            rows.push(Row::new(with_spine(vec![seg(meta, fg(DUSK.dim))]), body()));
            rows.extend(context_rows(input, checkpoint, context(checkpoint), &spine));
            rows.push(Row::new(spine.clone(), body()));
        }
        if is_selected {
            selected = Some((first, rows.len().saturating_sub(1)));
        }
    }
    if !any_visible {
        let text = if all.is_empty() {
            "  no checkpoints yet · b bookmarks this moment"
        } else {
            "  nothing of that kind yet"
        };
        rows.push(Row::new(vec![seg(text, italic(fg(DUSK.dim)))], None));
    }
    rows.push(Row::new(
        vec![seg(format!("{}┊", " ".repeat(6)), fg(DUSK.rule))],
        None,
    ));
    rows.push(Row::new(
        vec![
            seg("    ", fg(DUSK.dim)),
            seg("❧", fg(DUSK.gold)),
            seg(
                format!("  {} · now", hhmm(input.now, input.utc_offset)),
                italic(fg(DUSK.dim)),
            ),
        ],
        None,
    ));
    HistoryRows {
        pinned,
        rows,
        selected,
    }
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

/// `line[s..e]` with `` `code` `` spans lit; each segment maps clicks back to
/// its source offset.
fn src_segs(line: &str, s: usize, e: usize, base: Style, line_index: usize) -> Vec<Seg> {
    let text = &line[s..e];
    let mut out = Vec::new();
    let at = |offset: usize| InfoDockTarget::EditAt {
        line: line_index,
        byte_start: s + offset,
        spaced: false,
    };
    let mut cursor = 0;
    let mut search = 0;
    while let Some(open) = text[search..].find('`').map(|i| search + i) {
        let Some(close) = text[open + 1..].find('`').map(|i| open + 1 + i) else {
            break;
        };
        if open > cursor {
            out.push(hit(&text[cursor..open], base, at(cursor)));
        }
        out.push(hit(&text[open + 1..close], fg(DUSK.gold), at(open + 1)));
        cursor = close + 1;
        search = cursor;
    }
    if cursor < text.len() {
        out.push(hit(&text[cursor..], base, at(cursor)));
    }
    if out.is_empty() {
        out.push(seg("", base));
    }
    out
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

/// `- text`: the byte offset of the text.
fn parse_bullet(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start().len();
    line[indent..].strip_prefix("- ")?;
    Some(indent + 2)
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

/// The Notes view's read rows (`notes_rows` in the stub).
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
    for (li, raw) in input.text.split('\n').enumerate() {
        let line = raw.trim_end();
        let end = Some(InfoDockTarget::EditEnd {
            line: li,
            byte: line.len(),
        });
        if line.is_empty() {
            rows.push(Row::new(
                Vec::new(),
                Some(InfoDockTarget::EditEnd { line: li, byte: 0 }),
            ));
            continue;
        }
        if let Some(title) = line.strip_prefix("# ") {
            let title = title.trim();
            if title == input.tab_label.trim() {
                continue;
            }
            let at = line.len() - line[2..].trim_start().len();
            let shown = spaced(title);
            let fits = shown.width() + 2 <= width;
            rows.push(Row::new(
                vec![
                    hit(
                        "❦ ",
                        fg(DUSK.gold),
                        InfoDockTarget::EditEnd { line: li, byte: 0 },
                    ),
                    hit(
                        if fits { shown } else { title.to_owned() },
                        bold(fg(DUSK.ink)),
                        InfoDockTarget::EditAt {
                            line: li,
                            byte_start: at,
                            spaced: fits,
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
                end,
            ));
            continue;
        }
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
            for (index, (s, e)) in wrap_spans(line, width.saturating_sub(2).max(1), text_at)
                .into_iter()
                .enumerate()
            {
                let mut segs = if index == 0 {
                    vec![mark.clone()]
                } else {
                    vec![seg("  ", style)]
                };
                segs.extend(src_segs(line, s, e, style, li));
                rows.push(Row::new(
                    segs,
                    Some(InfoDockTarget::EditEnd { line: li, byte: e }),
                ));
            }
            continue;
        }
        if let Some(text_at) = parse_bullet(line) {
            let timed = parse_time(line, text_at);
            let start = if timed { text_at + 6 } else { text_at };
            let wrap_width = width.saturating_sub(if timed { 8 } else { 2 }).max(1);
            for (index, (s, e)) in wrap_spans(line, wrap_width, start).into_iter().enumerate() {
                let mut segs = match (timed, index) {
                    (true, 0) => vec![
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
                            fg(DUSK.dim),
                            InfoDockTarget::EditEnd {
                                line: li,
                                byte: start,
                            },
                        ),
                    ],
                    (true, _) => vec![seg(" ".repeat(8), fg(DUSK.dim))],
                    (false, 0) => vec![hit(
                        "· ",
                        fg(DUSK.rust),
                        InfoDockTarget::EditEnd {
                            line: li,
                            byte: text_at,
                        },
                    )],
                    (false, _) => vec![seg("  ", fg(DUSK.rust))],
                };
                segs.extend(src_segs(line, s, e, fg(DUSK.body), li));
                rows.push(Row::new(
                    segs,
                    Some(InfoDockTarget::EditEnd { line: li, byte: e }),
                ));
            }
            continue;
        }
        if let Some(keyword_len) = parse_keyword(line) {
            let lead = keyword_len + 2;
            let lead_width = line[..lead].width();
            for (index, (s, e)) in wrap_spans(line, width.saturating_sub(lead_width).max(1), lead)
                .into_iter()
                .enumerate()
            {
                let mut segs = if index == 0 {
                    vec![hit(
                        &line[..lead],
                        italic(fg(DUSK.note)),
                        InfoDockTarget::EditAt {
                            line: li,
                            byte_start: 0,
                            spaced: false,
                        },
                    )]
                } else {
                    vec![seg(" ".repeat(lead_width), fg(DUSK.note))]
                };
                segs.extend(src_segs(line, s, e, fg(DUSK.body), li));
                rows.push(Row::new(
                    segs,
                    Some(InfoDockTarget::EditEnd { line: li, byte: e }),
                ));
            }
            continue;
        }
        for (s, e) in wrap_spans(line, width.max(1), 0) {
            rows.push(Row::new(
                src_segs(line, s, e, fg(DUSK.body), li),
                Some(InfoDockTarget::EditEnd { line: li, byte: e }),
            ));
        }
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
