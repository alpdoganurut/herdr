//! The info dock (fork): a per-tab side pane, docked right and drawn by the
//! client, with the tab's agent session notes page. The visual and
//! interaction spec is the picked stub `c-almanac.py`; the plan is
//! `.local/prd/info-pane-design.md` §1.
//!
//! Notes are server facts (`notes.*`); everything here is client
//! presentation state: which tabs have the dock open, its width, focus,
//! scroll, the editor buffer and its dirty / conflict state.
//!
//! Lazy: `ClientShellState.info_dock` stays `None` until the first toggle,
//! and while the focused tab's dock is closed the tick returns at once, with
//! one exception: an editor buffer that still has to be saved.
//!
//! Requests go one at a time (`DockData.in_flight`), queued and coalesced
//! by kind, and only while no other endpoint command of this client is
//! pending, so the dock never delays the user's own commands by more than
//! the one request already on the wire. The client's endpoint lane already
//! serializes commands (`client/endpoint_commands.rs`), so no extra yield
//! is needed against `endpoint_busy`. Replies never raise a notice: errors
//! show in the dock body or footer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use super::info_dock_model::{self as model, InfoDockTarget, Row};

/// See `ClientShellState::surface_report_basis`.
#[derive(Debug)]
pub(crate) struct SurfaceReportBasis {
    pub(super) size: crate::protocol::ClientSurfaceSize,
    tab_id: Option<String>,
}
use super::info_dock_render::{self, DockFrame, CHROME_ROWS};
use super::*;
use crate::api::schema::notes::{
    NotesAuthor, NotesGetParams, NotesInfo, NotesSetParams, NotesTarget, NotesWriteOutcome,
};
use crate::api::schema::{Method, ResponseResult};
use crate::notes::BoundedMap;

/// The narrowest dock.
pub(super) const DOCK_MIN: u16 = 28;
/// The narrowest pane surface left beside the dock.
pub(super) const TERM_MIN: u16 = 30;
/// `<` / `>` step.
const DOCK_STEP: u16 = 4;
/// How often the active view is pulled while the dock is open.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Retry delay after `endpoint_busy`.
const BUSY_RETRY: Duration = Duration::from_millis(250);
/// A dirty editor saves after this long without a keystroke.
const IDLE_SAVE: Duration = Duration::from_secs(2);
const FLASH_FOR: Duration = Duration::from_secs(3);
const DIVIDER_DOUBLE_CLICK: Duration = Duration::from_millis(350);
const UI_CAP: usize = 32;

/// The dock's hit map: its area, the divider column, the scrolling body
/// (for the wheel) and every click target, last registered wins.
#[derive(Debug, Default, Clone)]
pub(crate) struct InfoDockHits {
    pub(crate) area: Rect,
    pub(crate) divider: Rect,
    pub(crate) body: Rect,
    pub(crate) targets: Vec<(Rect, InfoDockTarget)>,
}

impl InfoDockHits {
    /// The target under `point`; a segment beats its row.
    pub(crate) fn resolve(&self, point: (u16, u16)) -> Option<(Rect, &InfoDockTarget)> {
        self.targets
            .iter()
            .rev()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(rect, target)| (*rect, target))
    }
}

/// Per-session presentation state (keyed by session key).
#[derive(Debug, Default)]
pub(crate) struct DockUi {
    pub(crate) notes_scroll: usize,
    /// Keep the editor's cursor visible on the next
    /// `compute_view`.
    pub(crate) follow: bool,
}

/// One dock request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InfoRequest {
    NotesGet,
    NotesSet {
        key: String,
        text: String,
        base_revision: Option<String>,
        /// A task tick from the read view (a conflict asks to tick again).
        tick: bool,
    },
}

impl InfoRequest {
    fn same_slot(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::NotesGet, Self::NotesGet) | (Self::NotesSet { .. }, Self::NotesSet { .. })
        )
    }

    /// Requests that only mean something while the focused tab's dock is
    /// shown; a save by key does not.
    fn needs_open_dock(&self) -> bool {
        !matches!(self, Self::NotesSet { .. })
    }
}

#[derive(Debug)]
pub(crate) struct QueuedRequest {
    pub(crate) request: InfoRequest,
    pub(crate) not_before: Option<Instant>,
}

#[derive(Debug)]
pub(crate) struct InFlightRequest {
    pub(crate) request: InfoRequest,
    pub(crate) request_id: String,
}

/// What of the snapshot decides a fresh pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockSignature {
    pub(crate) boot_id: String,
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) tab_id: String,
    pub(crate) focused_pane_id: Option<String>,
}

/// What the server last said, for the focused tab.
#[derive(Debug, Default)]
pub(crate) struct DockData {
    /// The last `notes.get`; `text` is always filled (cleaned) once known.
    pub(crate) notes: Option<NotesInfo>,
    /// `notes.get` answered `not_found` (shows the empty state).
    pub(crate) notes_missing: bool,
    /// A body message instead of the notes (`too_large`).
    pub(crate) notes_error: Option<String>,
    pub(crate) in_flight: Option<InFlightRequest>,
    pub(crate) queue: VecDeque<QueuedRequest>,
    pub(crate) next_poll: Option<Instant>,
    pub(crate) pulled_for: Option<DockSignature>,
    /// A needed method is not advertised by the server.
    pub(crate) unsupported: bool,
    /// A reply said `notes_disabled`.
    pub(crate) disabled: bool,
    /// Bumps whenever anything rows are built from changes.
    pub(crate) generation: u64,
}

impl DockData {
    /// Forget what was shown (another tab, pane or server); queued saves
    /// survive, the rest of the queue does not.
    fn clear_shown(&mut self) {
        self.notes = None;
        self.notes_missing = false;
        self.notes_error = None;
        self.unsupported = false;
        self.disabled = false;
        self.queue
            .retain(|queued| matches!(queued.request, InfoRequest::NotesSet { .. }));
        self.generation = self.generation.wrapping_add(1);
    }

    /// The session key the shown data belongs to.
    fn key(&self) -> Option<&str> {
        self.notes.as_ref().map(|notes| notes.key.as_str())
    }
}

/// The in-dock notes editor (click-to-edit).
#[derive(Debug)]
pub(crate) struct NotesEditor {
    pub(crate) lines: Vec<String>,
    /// (line, byte offset), always a char boundary.
    pub(crate) cursor: (usize, usize),
    /// The revision the buffer was based on (`None` while no file exists).
    pub(crate) base_revision: Option<String>,
    /// The text of that revision, for the keep-mine merge.
    pub(crate) base_text: String,
    pub(crate) dirty: bool,
    pub(crate) last_edit: Instant,
    /// A save is queued or in flight.
    pub(crate) save_pending: bool,
    /// The text of the save in flight.
    pub(crate) sent_text: Option<String>,
    pub(crate) conflict: Option<NotesInfo>,
    /// The session key the buffer belongs to.
    pub(crate) key: String,
    /// Esc, a view switch or a close: drop the editor once saved.
    pub(crate) leaving: bool,
}

impl NotesEditor {
    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn touch(&mut self, now: Instant) {
        self.dirty = true;
        self.last_edit = now;
        self.leaving = false;
    }

    fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor.0.min(self.lines.len() - 1);
        let byte = model::floor_boundary(&self.lines[row], self.cursor.1);
        self.cursor = (row, byte);
    }

    fn insert(&mut self, text: &str, now: Instant) {
        self.clamp_cursor();
        let text = model::clean(text);
        let (row, byte) = self.cursor;
        let tail = self.lines[row].split_off(byte);
        let mut pieces = text.split('\n');
        if let Some(first) = pieces.next() {
            self.lines[row].push_str(first);
        }
        let mut row = row;
        for piece in pieces {
            row += 1;
            self.lines.insert(row, piece.to_owned());
        }
        let byte = self.lines[row].len();
        self.lines[row].push_str(&tail);
        self.cursor = (row, byte);
        self.touch(now);
    }

    /// One editing key; `false` when the key means nothing to the editor.
    fn apply_key(&mut self, key: EditorKey, now: Instant) -> bool {
        self.clamp_cursor();
        let (row, byte) = self.cursor;
        let line = &self.lines[row];
        match key {
            EditorKey::Up => {
                let row = row.saturating_sub(1);
                self.cursor = (row, model::floor_boundary(&self.lines[row], byte));
            }
            EditorKey::Down => {
                let row = (row + 1).min(self.lines.len() - 1);
                self.cursor = (row, model::floor_boundary(&self.lines[row], byte));
            }
            EditorKey::Left => {
                if let Some(ch) = line[..byte].chars().next_back() {
                    self.cursor = (row, byte - ch.len_utf8());
                } else if row > 0 {
                    self.cursor = (row - 1, self.lines[row - 1].len());
                }
            }
            EditorKey::Right => {
                if let Some(ch) = line[byte..].chars().next() {
                    self.cursor = (row, byte + ch.len_utf8());
                } else if row + 1 < self.lines.len() {
                    self.cursor = (row + 1, 0);
                }
            }
            EditorKey::Home => self.cursor = (row, 0),
            EditorKey::End => self.cursor = (row, line.len()),
            EditorKey::Backspace => {
                if let Some(ch) = line[..byte].chars().next_back() {
                    let at = byte - ch.len_utf8();
                    self.lines[row].replace_range(at..byte, "");
                    self.cursor = (row, at);
                } else if row > 0 {
                    let current = self.lines.remove(row);
                    let at = self.lines[row - 1].len();
                    self.lines[row - 1].push_str(&current);
                    self.cursor = (row - 1, at);
                } else {
                    return true;
                }
                self.touch(now);
            }
            EditorKey::Delete => {
                if let Some(ch) = line[byte..].chars().next() {
                    self.lines[row].replace_range(byte..byte + ch.len_utf8(), "");
                } else if row + 1 < self.lines.len() {
                    let next = self.lines.remove(row + 1);
                    self.lines[row].push_str(&next);
                } else {
                    return true;
                }
                self.touch(now);
            }
            EditorKey::Enter => {
                let tail = self.lines[row].split_off(byte);
                self.lines.insert(row + 1, tail);
                self.cursor = (row + 1, 0);
                self.touch(now);
            }
            EditorKey::Text(text) => self.insert(&text, now),
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditorKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    Enter,
    Text(String),
}

/// What `render` draws from the memo: the scrolling rows, the scroll and a
/// marker (body row, symbol).
type ViewSlices<'a> = (&'a [Row], usize, Option<(usize, &'static str)>);

/// The memoised rows of the view.
#[derive(Debug)]
enum CachedRows {
    Notes(Vec<Row>),
    Editor { rows: Vec<Row>, cursor_row: usize },
    Message(Vec<Row>),
}

/// When the memoised rows must be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowsStamp {
    width: u16,
    data: u64,
    ui: u64,
    editor: u64,
    labels: u64,
    /// The local UTC offset the `updated` time was drawn with (a DST
    /// change rebuilds; the clock's minute does not).
    utc_offset: i64,
}

#[derive(Debug)]
struct RowsCache {
    stamp: RowsStamp,
    rows: CachedRows,
}

/// The client-only dock state; `None` in the shell until the first toggle.
#[derive(Debug)]
pub(crate) struct ClientInfoDockState {
    pub(crate) open_tabs: HashSet<(ClientEndpointId, String)>,
    pub(crate) focused: bool,
    pub(crate) ui: BoundedMap<String, DockUi>,
    pub(crate) data: DockData,
    pub(crate) editor: Option<NotesEditor>,
    pub(crate) dragging: bool,
    pub(crate) last_divider_press: Option<Instant>,
    pub(crate) flash: Option<(String, Instant)>,
    /// The last `endpoint.notes-reveal.v1` acted on, per endpoint: its boot
    /// and revision.
    pub(crate) reveal_seen: HashMap<ClientEndpointId, (String, u64)>,
    /// Bumps on every change to `ui` (and on a session key change).
    ui_generation: u64,
    /// Bumps on every editor change.
    editor_generation: u64,
    rows: Option<RowsCache>,
    /// How often the rows were rebuilt (tests).
    #[cfg(test)]
    pub(crate) rows_built: u64,
    /// `ui_for`'s unreachable fallback.
    scratch_ui: DockUi,
}

impl Default for ClientInfoDockState {
    fn default() -> Self {
        Self {
            open_tabs: HashSet::new(),
            focused: false,
            ui: BoundedMap::new(UI_CAP),
            data: DockData::default(),
            editor: None,
            dragging: false,
            last_divider_press: None,
            flash: None,
            reveal_seen: HashMap::new(),
            ui_generation: 0,
            editor_generation: 0,
            rows: None,
            #[cfg(test)]
            rows_built: 0,
            scratch_ui: DockUi::default(),
        }
    }
}

/// What `compose` needs from the shell besides the dock state.
pub(super) struct DockComposeInput<'a> {
    pub(super) tab_label: &'a str,
    pub(super) sized_elsewhere: bool,
    /// Fork (session header): the tabs layout's title bar in place of the
    /// Notes strip.
    pub(super) title_bar: bool,
}

/// The local UTC offset in seconds (the one in effect now).
fn local_utc_offset() -> i64 {
    crate::platform::local_datetime()
        .map(|local| {
            let utc = time::OffsetDateTime::now_utc();
            let utc = time::PrimitiveDateTime::new(utc.date(), utc.time());
            ((local - utc).whole_seconds() + 30).div_euclid(60) * 60
        })
        .unwrap_or(0)
}

fn label_hash(label: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    label.hash(&mut hasher);
    hasher.finish()
}

impl ClientInfoDockState {
    /// The key presentation state is kept under: the shown session key,
    /// else the tab until the first reply names one.
    fn ui_key(&self) -> String {
        match self.data.key() {
            Some(key) => key.to_owned(),
            None => format!(
                "tab:{}",
                self.data
                    .pulled_for
                    .as_ref()
                    .map_or("", |pulled| pulled.tab_id.as_str())
            ),
        }
    }

    /// The presentation state under `key`, created on first use.
    fn ui_for(&mut self, key: &str) -> &mut DockUi {
        if !self.ui.contains_key(key) {
            self.ui.insert(key.to_owned(), DockUi::default());
        }
        match self.ui.get_mut(key) {
            Some(ui) => ui,
            // Inserted above; never reached.
            None => &mut self.scratch_ui,
        }
    }

    /// The editor is shown (not on its way out).
    pub(crate) fn editing(&self) -> bool {
        self.editor.as_ref().is_some_and(|editor| !editor.leaving)
    }

    fn bump_ui(&mut self) {
        self.ui_generation = self.ui_generation.wrapping_add(1);
    }

    fn bump_editor(&mut self) {
        self.editor_generation = self.editor_generation.wrapping_add(1);
    }

    fn bump_data(&mut self) {
        self.data.generation = self.data.generation.wrapping_add(1);
    }

    fn set_flash(&mut self, text: impl Into<String>, now: Instant) {
        self.flash = Some((text.into(), now));
    }

    /// Queue `request`, replacing a queued one of the same kind.
    pub(crate) fn queue(&mut self, request: InfoRequest, not_before: Option<Instant>) {
        if let Some(queued) = self
            .data
            .queue
            .iter_mut()
            .find(|queued| queued.request.same_slot(&request))
        {
            queued.request = request;
            queued.not_before = not_before;
            return;
        }
        self.data.queue.push_back(QueuedRequest {
            request,
            not_before,
        });
    }

    fn queue_view_pull(&mut self) {
        // While editing the save's conflict is the only source of truth.
        if self.editor.is_none() {
            self.queue(InfoRequest::NotesGet, None);
        }
    }

    /// Queue a save of a dirty buffer (`leaving`: drop the editor once it is
    /// saved). A clean buffer that is leaving goes at once.
    fn commit_editor(&mut self, leaving: bool) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        if leaving {
            editor.leaving = true;
        }
        if !editor.dirty && !editor.save_pending && editor.conflict.is_none() {
            if editor.leaving {
                self.editor = None;
                self.bump_editor();
            }
            return;
        }
        if editor.conflict.is_some() {
            // The user must pick reload or keep mine first.
            if leaving {
                editor.leaving = false;
            }
            return;
        }
        if !editor.dirty || editor.save_pending {
            return;
        }
        let request = InfoRequest::NotesSet {
            key: editor.key.clone(),
            text: editor.text(),
            base_revision: editor.base_revision.clone(),
            tick: false,
        };
        editor.save_pending = true;
        self.queue(request, None);
        self.bump_editor();
    }

    /// Start editing the shown notes with the cursor at `at` (line, byte),
    /// or at the end.
    fn start_edit(&mut self, at: Option<(usize, usize)>, now: Instant) -> bool {
        if let Some(editor) = self.editor.as_mut() {
            // A buffer still on its way out comes back as it was.
            editor.leaving = false;
            if let Some((line, byte)) = at {
                editor.cursor = (line, byte);
                editor.clamp_cursor();
            }
            self.bump_editor();
            return true;
        }
        let Some(notes) = self.data.notes.as_ref() else {
            self.set_flash("notes are still loading", now);
            return false;
        };
        let text = notes.text.clone().unwrap_or_default();
        let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        let cursor = match at {
            Some(at) => at,
            None => {
                // The end of the last non-empty line.
                let row = lines.iter().rposition(|line| !line.is_empty()).unwrap_or(0);
                (row, lines[row].len())
            }
        };
        let base_revision =
            (notes.exists && notes.revision != "none").then(|| notes.revision.clone());
        let mut editor = NotesEditor {
            lines,
            cursor,
            base_revision,
            base_text: text,
            dirty: false,
            last_edit: now,
            save_pending: false,
            sent_text: None,
            conflict: None,
            key: notes.key.clone(),
            leaving: false,
        };
        editor.clamp_cursor();
        self.editor = Some(editor);
        self.bump_editor();
        true
    }

    /// Rebuild the active view's rows when their stamp changed, then clamp
    /// or follow the scroll (`compute_view` in the stub: scroll is the only
    /// mutation besides the memo).
    pub(super) fn compute_view(
        &mut self,
        area: Rect,
        input: &DockComposeInput<'_>,
        utc_offset: i64,
    ) {
        let width = area.width.saturating_sub(2);
        let stamp = RowsStamp {
            width,
            data: self.data.generation,
            ui: self.ui_generation,
            editor: self.editor_generation,
            labels: label_hash(input.tab_label),
            utc_offset,
        };
        let key = self.ui_key();
        if self.rows.as_ref().is_none_or(|cache| cache.stamp != stamp) {
            let rows = self.build_rows(usize::from(width), input, utc_offset);
            self.rows = Some(RowsCache { stamp, rows });
            #[cfg(test)]
            {
                self.rows_built += 1;
            }
        }
        let avail = usize::from(area.height.saturating_sub(CHROME_ROWS));
        let Some(cache) = self.rows.as_ref() else {
            return;
        };
        let (count, span) = match &cache.rows {
            CachedRows::Notes(rows) | CachedRows::Message(rows) => (rows.len(), None),
            CachedRows::Editor { rows, cursor_row } => {
                (rows.len(), Some((*cursor_row, *cursor_row)))
            }
        };
        let avail = avail.max(1);
        let ui = self.ui_for(&key);
        let scroll = &mut ui.notes_scroll;
        let mut next = *scroll;
        if ui.follow {
            if let Some((first, last)) = span {
                if first < next {
                    next = first;
                }
                if last >= next + avail {
                    next = if last - first < avail {
                        (first.saturating_sub(1)).max(last + 1 - avail)
                    } else {
                        first
                    };
                }
                if first < next {
                    next = first;
                }
            }
        }
        *scroll = next.min(count.saturating_sub(avail));
        ui.follow = false;
    }

    fn build_rows(
        &mut self,
        width: usize,
        input: &DockComposeInput<'_>,
        utc_offset: i64,
    ) -> CachedRows {
        let message =
            |text: &str, retry: bool| CachedRows::Message(model::message_rows(text, retry, width));
        if self.data.unsupported {
            return message(
                "not available on this server (update it to use the info pane)",
                false,
            );
        }
        if self.data.disabled {
            return message(
                "notes are turned off on this server ([notes] enabled = false)",
                false,
            );
        }
        if let Some(editor) = self.editor.as_ref().filter(|editor| !editor.leaving) {
            let (rows, cursor_row) = model::editor_rows(&editor.lines, editor.cursor, width);
            return CachedRows::Editor { rows, cursor_row };
        }
        if let Some(error) = self.data.notes_error.as_deref() {
            return message(error, true);
        }
        let Some(notes) = self.data.notes.as_ref() else {
            return message(
                if self.data.notes_missing {
                    "no notes here"
                } else {
                    "loading…"
                },
                false,
            );
        };
        CachedRows::Notes(model::notes_rows(&model::NotesInput {
            text: notes.text.as_deref().unwrap_or_default(),
            exists: notes.exists,
            tab_label: input.tab_label,
            revision: &notes.revision,
            updated_by: notes.updated_by,
            updated_at: notes.updated_at,
            utc_offset,
            width,
        }))
    }

    /// Draw the dock into `buffer` at `area` (after `compute_view`).
    fn render(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        input: &DockComposeInput<'_>,
        key: &str,
    ) -> InfoDockHits {
        let width = usize::from(area.width.saturating_sub(2));
        let strip = if input.title_bar {
            model::title_bar(width, self.editing())
        } else {
            model::tab_strip(width)
        };
        let conflict = self.editor.as_ref().and_then(|editor| {
            editor.conflict.as_ref().map(|theirs| {
                model::theirs_added(
                    &editor.base_text,
                    &editor.lines,
                    theirs.text.as_deref().unwrap_or_default(),
                )
            })
        });
        let footer = model::footer(
            &model::FooterInput {
                editing: self.editing(),
                dragging: self.dragging,
                flash: self.flash.as_ref().map(|(text, _)| text.as_str()),
                conflict,
                sized_elsewhere: input.sized_elsewhere,
            },
            width,
        );
        let avail = usize::from(area.height.saturating_sub(CHROME_ROWS));
        let empty = DockUi::default();
        let ui = self.ui.peek(key).unwrap_or(&empty);
        let mut hits = InfoDockHits::default();
        let Some(cache) = self.rows.as_ref() else {
            return hits;
        };
        let (rows, scroll, mark): ViewSlices<'_> = match &cache.rows {
            CachedRows::Editor { rows, cursor_row } => {
                (rows, ui.notes_scroll, Some((*cursor_row, "▸")))
            }
            CachedRows::Notes(rows) | CachedRows::Message(rows) => (rows, ui.notes_scroll, None),
        };
        let body_avail = avail;
        let start = scroll.min(rows.len());
        let end = (start + body_avail).min(rows.len());
        let mark = mark
            .filter(|(row, _)| (start..end).contains(row))
            .map(|(row, symbol)| (row - start, symbol));
        info_dock_render::render(
            buffer,
            area,
            &DockFrame {
                strip: &strip,
                body: &rows[start..end],
                mark,
                footer: &footer,
                dragging: self.dragging,
            },
            &mut hits,
        );
        hits
    }

    /// Compute the view and draw it; returns the hit map.
    pub(super) fn compose(
        &mut self,
        buffer: &mut Buffer,
        area: Rect,
        input: &DockComposeInput<'_>,
    ) -> InfoDockHits {
        self.compute_view(area, input, local_utc_offset());
        let key = self.ui_key();
        self.render(buffer, area, input, &key)
    }
}

impl ClientShellState {
    // ------------------------------------------------------------ queries

    /// The tab the pane surface shows and its size, before a snapshot lands
    /// (`surface_report_required` compares it with the state after).
    pub(crate) fn surface_report_basis(&self, cols: u16, rows: u16) -> SurfaceReportBasis {
        SurfaceReportBasis {
            size: self.surface_size(cols, rows),
            tab_id: self.info_dock_focused_tab().map(str::to_owned),
        }
    }

    /// Whether the client reports its surface size after a snapshot: the
    /// size changed, or the surface now shows another tab. The server sizes
    /// only the tab shown (`ShellTabSizing`), so a newly shown tab gets the
    /// size even when it is unchanged (a no-op server side when the tab
    /// already has it).
    pub(crate) fn surface_report_required(
        &self,
        before: &SurfaceReportBasis,
        cols: u16,
        rows: u16,
    ) -> bool {
        before.size != self.surface_size(cols, rows)
            || (before.tab_id.is_some() && before.tab_id.as_deref() != self.info_dock_focused_tab())
    }

    fn info_dock_focused_tab(&self) -> Option<&str> {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_tab_id.as_deref())
    }

    /// Whether the focused tab of the active endpoint has its dock open.
    pub(super) fn info_dock_open_for_focused_tab(&self) -> bool {
        let Some(dock) = self.info_dock.as_deref() else {
            return false;
        };
        let Some(tab_id) = self.info_dock_focused_tab() else {
            return false;
        };
        dock.open_tabs
            .contains(&(self.active_endpoint_id.clone(), tab_id.to_owned()))
    }

    /// The dock width to carve for the focused tab, `None` when its dock is
    /// closed (the layout still drops a dock that does not fit).
    pub(super) fn info_dock_width_for_focused_tab(&self) -> Option<u16> {
        self.info_dock_open_for_focused_tab()
            .then_some(self.info_dock_width)
    }

    /// The dock's rect in the last composed layout (empty when not drawn).
    pub(super) fn info_dock_area(&self) -> Rect {
        if self.info_dock.is_none() {
            return Rect::default();
        }
        self.last_composed_size
            .map(|(cols, rows)| self.layout(cols, rows).info_dock)
            .unwrap_or_default()
    }

    /// The dock takes keys: it has focus and is drawn for the focused tab.
    pub(super) fn info_dock_focused(&self) -> bool {
        self.info_dock.as_deref().is_some_and(|dock| dock.focused)
            && !self.info_dock_area().is_empty()
    }

    /// The dock's editor takes text.
    #[cfg(test)]
    pub(super) fn info_dock_editing(&self) -> bool {
        self.info_dock_focused()
            && self
                .info_dock
                .as_deref()
                .is_some_and(ClientInfoDockState::editing)
    }

    fn info_dock_signature(&self) -> Option<DockSignature> {
        let snapshot = self.snapshot.as_deref()?;
        Some(DockSignature {
            boot_id: snapshot.boot_id.clone(),
            endpoint_id: self.active_endpoint_id.clone(),
            tab_id: snapshot.focused_tab_id.clone()?,
            focused_pane_id: snapshot.focused_pane_id.clone(),
        })
    }

    /// The widest dock the current window allows (`None` before a frame).
    fn info_dock_max_width(&self) -> Option<u16> {
        let (cols, rows) = self.last_composed_size?;
        let layout = self.layout(cols, rows);
        if !layout.mobile_header.is_empty() {
            return None;
        }
        let total = layout.pane_surface.width + layout.info_dock.width;
        Some(
            (cols * 7 / 10)
                .min(total.saturating_sub(TERM_MIN))
                .max(DOCK_MIN),
        )
    }

    // ------------------------------------------------------------ toggle

    /// Open or close the dock of `tab_id` on the active endpoint, never
    /// moving keyboard focus into it (closing commits the editor and drops
    /// the dock's focus). A tab that is not focused changes state only.
    pub(super) fn toggle_info_pane(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
        let size_before = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        let focused = self.info_dock_focused_tab() == Some(tab_id.as_str());
        let endpoint = self.active_endpoint_id.clone();
        // Fork (session header): one dock slot in the tabs layout; the
        // browser gives way only to notes that fit the window (else it stays
        // and the notice says why).
        if !self
            .info_dock
            .as_deref()
            .is_some_and(|dock| dock.open_tabs.contains(&(endpoint.clone(), tab_id.clone())))
        {
            if focused
                && self.config.sidebar_layout == crate::config::SidebarLayoutConfig::Tabs
                && self.browser_dock_open_for(&tab_id)
                && !self.info_dock_fits_alone()
            {
                outcome.repaint |= self.push_endpoint_notice(
                    ClientEndpointNoticeKind::Rejected,
                    "info_pane:narrow",
                    "Info pane",
                    "The window is too narrow for the info pane; it appears once the window is wider.",
                );
                return;
            }
            self.free_dock_slot_of_browser(&endpoint, &tab_id);
        }
        let key = (endpoint, tab_id);
        let dock = self.info_dock.get_or_insert_with(Box::default);
        let opened = if dock.open_tabs.remove(&key) {
            if focused {
                dock.commit_editor(true);
                dock.focused = false;
                dock.dragging = false;
            }
            false
        } else {
            dock.open_tabs.insert(key);
            // Opening never takes the keyboard, and drops a focus left from
            // another tab's dock: the session keeps it (a click into the
            // dock focuses it).
            dock.focused = false;
            dock.dragging = false;
            if focused {
                dock.flash = None;
                dock.data.pulled_for = None;
            }
            true
        };
        if !focused {
            outcome.repaint = true;
            return;
        }
        let size_after = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        if size_before != size_after {
            // The old frame stays up (its blit is clipped to the narrower
            // surface) until the resized one arrives; the dock only changes
            // the surface's width, never its origin.
            outcome.resize = true;
        }
        outcome.repaint = true;
        if opened && self.last_composed_size.is_some() && self.info_dock_area().is_empty() {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "info_pane:narrow",
                "Info pane",
                "The window is too narrow for the info pane; it appears once the window is wider.",
            );
        }
        let now = Instant::now();
        self.tick_info_dock(now, outcome);
    }

    /// An `endpoint.notes-reveal.v1` push (an agent's `agents_notes_write
    /// reveal=true`): open that tab's dock unless it is open, never taking
    /// the keyboard (a tab of another endpoint, or not focused, opens in
    /// state only). Each revision acts once per server boot.
    pub(crate) fn receive_notes_reveal(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: crate::server::headless::notes_reveal::NotesRevealPayload,
    ) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        let dock = self.info_dock.get_or_insert_with(Box::default);
        if dock
            .reveal_seen
            .get(endpoint_id)
            .is_some_and(|(boot_id, revision)| {
                *boot_id == payload.boot_id && *revision >= payload.revision
            })
        {
            return outcome;
        }
        dock.reveal_seen.insert(
            endpoint_id.clone(),
            (payload.boot_id.clone(), payload.revision),
        );
        let key = (endpoint_id.clone(), payload.tab_id.clone());
        if dock.open_tabs.contains(&key) {
            return outcome;
        }
        if *endpoint_id == self.active_endpoint_id {
            self.toggle_info_pane(payload.tab_id, &mut outcome);
        } else {
            dock.open_tabs.insert(key);
            dock.focused = false;
            dock.dragging = false;
            self.free_dock_slot_of_browser(endpoint_id, &payload.tab_id);
        }
        outcome
    }

    /// Fork (session header): whether the info dock would be drawn at the
    /// last composed size with no browser band beside it (`true` before a
    /// frame).
    pub(crate) fn info_dock_fits_alone(&self) -> bool {
        let Some((cols, rows)) = self.last_composed_size else {
            return true;
        };
        !self
            .config
            .layout(
                cols,
                rows,
                self.sidebar_collapsed,
                self.focused_tab_count(),
                self.sidebar_width,
                Some(self.info_dock_width),
            )
            .info_dock
            .is_empty()
    }

    /// Fork (session header): the focused tab's notes hold the tabs
    /// layout's dock slot.
    pub(crate) fn dock_slot_holds_notes(&self, tab_id: &str) -> bool {
        self.config.sidebar_layout == crate::config::SidebarLayoutConfig::Tabs
            && self.info_dock.as_deref().is_some_and(|dock| {
                dock.open_tabs
                    .contains(&(self.active_endpoint_id.clone(), tab_id.to_owned()))
            })
    }

    /// Fork (session header): a live switch to the tabs layout leaves one
    /// dock per tab: a tab with both keeps the browser, its notes close
    /// (the editor committed; the tick sends a dirty buffer's save even
    /// with the dock closed).
    pub(crate) fn reconcile_dock_slot(&mut self) {
        if self.config.sidebar_layout != crate::config::SidebarLayoutConfig::Tabs
            || self.browser_dock.docked.is_empty()
        {
            return;
        }
        let both: Vec<(ClientEndpointId, String)> = self
            .info_dock
            .as_deref()
            .map(|dock| {
                dock.open_tabs
                    .iter()
                    .filter(|(endpoint, tab_id)| {
                        self.browser_dock
                            .docked
                            .get(endpoint)
                            .is_some_and(|tabs| tabs.contains(tab_id))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for (endpoint, tab_id) in both {
            self.free_dock_slot_of_notes(&endpoint, &tab_id);
        }
    }

    /// Fork (session header): the tabs layout has one dock slot, so the
    /// browser dock opening closes `tab_id`'s notes dock (state only; the
    /// caller resizes), committing its editor. Returns whether it was open.
    pub(crate) fn free_dock_slot_of_notes(
        &mut self,
        endpoint: &ClientEndpointId,
        tab_id: &str,
    ) -> bool {
        if self.config.sidebar_layout != crate::config::SidebarLayoutConfig::Tabs {
            return false;
        }
        let focused =
            self.info_dock_focused_tab() == Some(tab_id) && *endpoint == self.active_endpoint_id;
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return false;
        };
        if !dock
            .open_tabs
            .remove(&(endpoint.clone(), tab_id.to_owned()))
        {
            return false;
        }
        if focused {
            dock.commit_editor(true);
            dock.focused = false;
            dock.dragging = false;
        }
        true
    }

    /// Whether `tab_id` of the active endpoint has its dock open while the
    /// dock does not have the keyboard (server-side smoke tests).
    #[cfg(test)]
    pub(crate) fn info_dock_open_unfocused_for_test(&self, tab_id: &str) -> bool {
        self.info_dock.as_deref().is_some_and(|dock| {
            !dock.focused
                && dock
                    .open_tabs
                    .contains(&(self.active_endpoint_id.clone(), tab_id.to_owned()))
        })
    }

    /// The `toggle_info_pane` keybind: the focused tab.
    pub(super) fn toggle_info_pane_for_focused_tab(&mut self, outcome: &mut ClientShellInput) {
        if let Some(tab_id) = self.info_dock_focused_tab().map(str::to_owned) {
            self.toggle_info_pane(tab_id, outcome);
        }
    }

    /// Keep only open docks whose tab the active endpoint still lists.
    pub(super) fn prune_info_dock_tabs(&mut self, snapshot: &ClientShellSnapshot) {
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return;
        };
        let endpoint_id = &self.active_endpoint_id;
        dock.open_tabs.retain(|(endpoint, tab_id)| {
            endpoint != endpoint_id || snapshot.tabs.iter().any(|tab| &tab.tab_id == tab_id)
        });
    }

    // ------------------------------------------------------------ width

    fn set_info_dock_width(&mut self, width: u16, outcome: &mut ClientShellInput) -> bool {
        let max = self.info_dock_max_width().unwrap_or(width.max(DOCK_MIN));
        let width = width.clamp(DOCK_MIN, max.max(DOCK_MIN));
        if width == self.info_dock_width {
            return false;
        }
        self.info_dock_width = width;
        self.info_dock_width_manual = true;
        // No surface invalidation: the dock keeps drawing while dragged.
        outcome.repaint = true;
        outcome.resize = true;
        true
    }

    /// A drag of the divider to `column`.
    pub(super) fn info_dock_drag(&mut self, column: u16, outcome: &mut ClientShellInput) {
        let area = self.info_dock_area();
        let right = if area.is_empty() {
            self.last_composed_size.map_or(0, |(cols, _)| cols)
        } else {
            area.right()
        };
        if self.set_info_dock_width(right.saturating_sub(column), outcome) {
            // A drag is no first click of a double-click.
            if let Some(dock) = self.info_dock.as_deref_mut() {
                dock.last_divider_press = None;
            }
        }
    }

    /// The divider drag ended: keep and remember the width.
    pub(super) fn info_dock_release(&mut self, outcome: &mut ClientShellInput) {
        let width = self.info_dock_area().width;
        if let Some(dock) = self.info_dock.as_deref_mut() {
            dock.dragging = false;
            dock.set_flash(format!("dock width {width}"), Instant::now());
        }
        outcome.repaint = true;
        self.persist_chrome_preferences(outcome);
    }

    fn step_info_dock_width(&mut self, wider: bool, outcome: &mut ClientShellInput) {
        let current = self.info_dock_area().width.max(DOCK_MIN);
        let width = if wider {
            current.saturating_add(DOCK_STEP)
        } else {
            current.saturating_sub(DOCK_STEP)
        };
        self.set_info_dock_width(width, outcome);
        self.persist_chrome_preferences(outcome);
        let shown = self.info_dock_width;
        if let Some(dock) = self.info_dock.as_deref_mut() {
            dock.set_flash(format!("dock width {shown}"), Instant::now());
        }
        outcome.repaint = true;
    }

    // ------------------------------------------------------------ mouse

    /// A left press inside the dock (checked before any pane hit). Returns
    /// whether the press was the dock's.
    pub(super) fn info_dock_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let hits = &self.hits.info_dock;
        if hits.area.is_empty() || !super::contains(hits.area, point) {
            return false;
        }
        let now = Instant::now();
        if super::contains(hits.divider, point) {
            let double_click = self
                .info_dock
                .as_deref()
                .and_then(|dock| dock.last_divider_press)
                .is_some_and(|last| now.duration_since(last) <= DIVIDER_DOUBLE_CLICK);
            if double_click {
                if let Some(dock) = self.info_dock.as_deref_mut() {
                    dock.last_divider_press = None;
                    dock.dragging = false;
                }
                let default = self.config.info_pane_width;
                let max = self.info_dock_max_width().unwrap_or(default);
                self.info_dock_width = default.clamp(DOCK_MIN, max.max(DOCK_MIN));
                self.info_dock_width_manual = false;
                outcome.resize = true;
                outcome.repaint = true;
                self.persist_chrome_preferences(outcome);
                return true;
            }
            // Start from the drawn width, not a remembered one the window
            // clamped.
            let drawn = hits.area.width;
            self.info_dock_width = drawn;
            if let Some(dock) = self.info_dock.as_deref_mut() {
                dock.last_divider_press = Some(now);
                dock.dragging = true;
            }
            self.chrome_drag = Some(ClientChromeDrag::InfoDockWidth);
            outcome.repaint = true;
            return true;
        }
        let target = hits
            .resolve(point)
            .map(|(rect, target)| (rect, target.clone()));
        if let Some(dock) = self.info_dock.as_deref_mut() {
            dock.focused = true;
            dock.flash = None;
        }
        outcome.repaint = true;
        if let Some((rect, target)) = target {
            let dx = usize::from(point.0.saturating_sub(rect.x));
            self.info_dock_activate(target, dx, now, outcome);
        }
        true
    }

    /// The wheel over the dock's body: scroll the active view.
    pub(super) fn info_dock_wheel(
        &mut self,
        point: (u16, u16),
        down: bool,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if self.hits.info_dock.area.is_empty() || !super::contains(self.hits.info_dock.area, point)
        {
            return false;
        }
        let lines = self.config.mouse_scroll_lines.max(1);
        if let Some(dock) = self.info_dock.as_deref_mut() {
            let key = dock.ui_key();
            let ui = dock.ui_for(&key);
            let scroll = &mut ui.notes_scroll;
            *scroll = if down {
                scroll.saturating_add(lines)
            } else {
                scroll.saturating_sub(lines)
            };
            ui.follow = false;
            outcome.repaint = true;
        }
        true
    }

    /// A press on a pane: the dock lets go of the keyboard and a dirty
    /// buffer is saved.
    pub(super) fn info_dock_pane_pressed(&mut self, outcome: &mut ClientShellInput) {
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return;
        };
        if !dock.focused && dock.editor.is_none() {
            return;
        }
        dock.focused = false;
        dock.commit_editor(false);
        outcome.repaint = true;
        self.pump_info_dock(Instant::now(), outcome);
    }

    /// Run a click target (the stub's `handle_mouse`).
    fn info_dock_activate(
        &mut self,
        target: InfoDockTarget,
        dx: usize,
        now: Instant,
        outcome: &mut ClientShellInput,
    ) {
        if target == InfoDockTarget::Close {
            self.toggle_info_pane_for_focused_tab(outcome);
            return;
        }
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return;
        };
        match target {
            InfoDockTarget::Close => {}
            InfoDockTarget::Edit => {
                if dock.editing() {
                    dock.commit_editor(true);
                    dock.bump_editor();
                } else if dock.start_edit(None, now) {
                    let key = dock.ui_key();
                    dock.ui_for(&key).follow = true;
                }
            }
            InfoDockTarget::Task { line, revision } => dock_tick_task(dock, line, &revision, now),
            InfoDockTarget::EditAt {
                line,
                byte_start,
                spaced,
            } => {
                let byte = dock_line(dock, line)
                    .map(|text| model::edit_offset(text, byte_start, dx, spaced))
                    .unwrap_or(byte_start);
                dock_edit_at(dock, line, byte, now);
            }
            InfoDockTarget::EditEnd { line, byte } => dock_edit_at(dock, line, byte, now),
            InfoDockTarget::ConflictReload => dock_conflict_reload(dock),
            InfoDockTarget::ConflictKeepMine => dock_conflict_keep_mine(dock),
            InfoDockTarget::Retry => {
                dock.data.notes_error = None;
                dock.data.unsupported = false;
                dock.bump_data();
                dock.queue_view_pull();
            }
        }
        outcome.repaint = true;
        self.pump_info_dock(now, outcome);
    }

    /// Tests: run a click target as if it was clicked at its first column.
    #[cfg(test)]
    pub(super) fn info_dock_activate_for_test(
        &mut self,
        target: InfoDockTarget,
        outcome: &mut ClientShellInput,
    ) {
        self.info_dock_activate(target, 0, Instant::now(), outcome);
    }

    // ------------------------------------------------------------ keys

    /// A key while the dock has focus (prefix and direct bindings were
    /// matched before). While editing every key but Esc edits.
    pub(super) fn handle_info_dock_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        use crossterm::event::{KeyCode, KeyModifiers};

        let now = Instant::now();
        outcome.repaint = true;
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        let text = |c: char| {
            key.generated_text
                .clone()
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| c.to_string())
        };
        let editing = self
            .info_dock
            .as_deref()
            .is_some_and(ClientInfoDockState::editing);
        if editing {
            let editor_key = match key.code {
                KeyCode::Esc => {
                    if let Some(dock) = self.info_dock.as_deref_mut() {
                        dock.commit_editor(true);
                        dock.bump_editor();
                    }
                    self.pump_info_dock(now, outcome);
                    return;
                }
                KeyCode::Up => EditorKey::Up,
                KeyCode::Down => EditorKey::Down,
                KeyCode::Left => EditorKey::Left,
                KeyCode::Right => EditorKey::Right,
                KeyCode::Home => EditorKey::Home,
                KeyCode::End => EditorKey::End,
                KeyCode::Backspace => EditorKey::Backspace,
                KeyCode::Delete => EditorKey::Delete,
                KeyCode::Enter => EditorKey::Enter,
                KeyCode::Tab => EditorKey::Text("    ".into()),
                KeyCode::Char(c) if plain => EditorKey::Text(text(c)),
                _ => return,
            };
            if let Some(dock) = self.info_dock.as_deref_mut() {
                if let Some(editor) = dock.editor.as_mut() {
                    editor.apply_key(editor_key, now);
                }
                dock.ui_for_current_follow();
                dock.bump_editor();
            }
            return;
        }
        let code = match key.code {
            KeyCode::Char(c) if plain => KeyCode::Char(c),
            KeyCode::Char(_) => return,
            code => code,
        };
        match code {
            KeyCode::Char('<') => {
                self.step_info_dock_width(false, outcome);
                return;
            }
            KeyCode::Char('>') => {
                self.step_info_dock_width(true, outcome);
                return;
            }
            KeyCode::Esc => {
                if let Some(dock) = self.info_dock.as_deref_mut() {
                    dock.focused = false;
                }
                return;
            }
            _ => {}
        }
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return;
        };
        dock.flash = None;
        let key_name = dock.ui_key();
        match code {
            KeyCode::Char('e') => {
                dock.start_edit(None, now);
                dock.ui_for(&key_name).follow = true;
            }
            KeyCode::Up | KeyCode::Char('k') => dock_scroll(dock, &key_name, false, 1),
            KeyCode::Down | KeyCode::Char('j') => dock_scroll(dock, &key_name, true, 1),
            KeyCode::PageUp => dock_scroll(dock, &key_name, false, 10),
            KeyCode::PageDown => dock_scroll(dock, &key_name, true, 10),
            _ => {}
        }
        self.pump_info_dock(now, outcome);
    }

    /// Paste or committed text while the dock has focus: into the editor
    /// when editing, else dropped (it never reaches the pane).
    pub(super) fn info_dock_insert_text(&mut self, text: &str) {
        let now = Instant::now();
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return;
        };
        if !dock.editing() {
            return;
        }
        if let Some(editor) = dock.editor.as_mut() {
            editor.insert(text, now);
        }
        dock.ui_for_current_follow();
        dock.bump_editor();
    }

    // ------------------------------------------------------------ tick

    /// Pull, poll, save and send. Returns at once while the focused tab's
    /// dock is closed and nothing is left to save.
    pub(crate) fn tick_info_dock(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        let Some(dock) = self.info_dock.as_deref() else {
            return;
        };
        let open = self.info_dock_open_for_focused_tab();
        let unsaved = dock
            .editor
            .as_ref()
            .is_some_and(|editor| editor.dirty || editor.save_pending);
        if !open && !unsaved && dock.data.in_flight.is_none() && dock.flash.is_none() {
            return;
        }
        let orphaned =
            dock.data.in_flight.as_ref().is_some_and(|in_flight| {
                !self.pending_requests.contains_key(&in_flight.request_id)
            });
        let signature = self.info_dock_signature();
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return;
        };
        if orphaned {
            // The projection was reset (a reboot, an endpoint switch): the
            // reply will never come.
            if let Some(in_flight) = dock.data.in_flight.take() {
                if let InfoRequest::NotesSet { tick: false, .. } = in_flight.request {
                    if let Some(editor) = dock.editor.as_mut() {
                        editor.save_pending = false;
                        editor.sent_text = None;
                    }
                }
            }
        }
        if dock
            .flash
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) >= FLASH_FOR)
        {
            dock.flash = None;
            outcome.repaint = true;
        }
        if open {
            if let Some(signature) = signature {
                if dock.data.pulled_for.as_ref() != Some(&signature) {
                    let boot_changed = dock
                        .data
                        .pulled_for
                        .as_ref()
                        .is_some_and(|pulled| pulled.boot_id != signature.boot_id);
                    if boot_changed {
                        dock.data.in_flight = None;
                        dock.data.queue.clear();
                        if let Some(editor) = dock.editor.as_mut() {
                            editor.save_pending = false;
                            editor.sent_text = None;
                        }
                    }
                    dock.data.clear_shown();
                    dock.bump_ui();
                    dock.data.pulled_for = Some(signature);
                    dock.queue_view_pull();
                    dock.data.next_poll = Some(now + POLL_INTERVAL);
                    outcome.repaint = true;
                } else if dock.data.next_poll.is_none_or(|at| now >= at) {
                    dock.queue_view_pull();
                    dock.data.next_poll = Some(now + POLL_INTERVAL);
                }
            }
        }
        if let Some(editor) = dock.editor.as_ref() {
            if editor.dirty
                && !editor.save_pending
                && editor.conflict.is_none()
                && now.duration_since(editor.last_edit) >= IDLE_SAVE
            {
                dock.commit_editor(false);
            }
        }
        self.pump_info_dock(now, outcome);
    }

    /// When the tick must wake for the dock: `None` while it is closed and
    /// nothing is left to save.
    pub(crate) fn next_info_dock_deadline(&self, now: Instant) -> Option<Instant> {
        let dock = self.info_dock.as_deref()?;
        let open = self.info_dock_open_for_focused_tab();
        let unsaved = dock
            .editor
            .as_ref()
            .is_some_and(|editor| editor.dirty || editor.save_pending);
        if !open && !unsaved {
            return None;
        }
        let flash = dock.flash.as_ref().map(|(_, at)| *at + FLASH_FOR);
        let idle_save = dock
            .editor
            .as_ref()
            .filter(|editor| editor.dirty && !editor.save_pending && editor.conflict.is_none())
            .map(|editor| editor.last_edit + IDLE_SAVE);
        let queued = dock
            .data
            .queue
            .iter()
            .filter_map(|queued| queued.not_before)
            .min();
        let poll = open.then_some(dock.data.next_poll).flatten();
        // Only future instants: anything due is handled by the next tick, and
        // a past deadline (a request waiting on another command) would spin
        // the client loop.
        [flash, idle_save, queued, poll]
            .into_iter()
            .flatten()
            .filter(|at| *at > now)
            .min()
    }

    /// Send the next queued request when the slot is free: nothing of the
    /// dock in flight and no other command of this client pending.
    pub(super) fn pump_info_dock(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        let open = self.info_dock_open_for_focused_tab();
        let tab_id = self.info_dock_focused_tab().map(str::to_owned);
        let Some(dock) = self.info_dock.as_deref() else {
            return;
        };
        if dock.data.in_flight.is_some() || dock.data.queue.is_empty() {
            return;
        }
        if self
            .pending_requests
            .values()
            .any(|pending| !pending.kind.is_info_dock())
            || !self.endpoint_is_online(&self.active_endpoint_id)
        {
            return;
        }
        loop {
            let Some(dock) = self.info_dock.as_deref_mut() else {
                return;
            };
            if !open {
                dock.data
                    .queue
                    .retain(|queued| !queued.request.needs_open_dock());
            }
            let Some(index) = dock
                .data
                .queue
                .iter()
                .position(|queued| queued.not_before.is_none_or(|at| now >= at))
            else {
                return;
            };
            let Some(queued) = dock.data.queue.remove(index) else {
                return;
            };
            let Some((method, kind)) =
                info_request_method(dock, &queued.request, tab_id.as_deref())
            else {
                continue;
            };
            if !self.supports_endpoint_method(&method) {
                if let Some(dock) = self.info_dock.as_deref_mut() {
                    if queued.request.needs_open_dock() {
                        dock.data.unsupported = true;
                        dock.bump_data();
                    } else if let Some(editor) = dock.editor.as_mut() {
                        editor.save_pending = false;
                    }
                }
                outcome.repaint = true;
                continue;
            }
            let request_id = format!("client-shell:{}", self.next_request_id);
            if self.push_endpoint_method_with_kind(method, kind, outcome) {
                if let Some(dock) = self.info_dock.as_deref_mut() {
                    if let InfoRequest::NotesSet {
                        text, tick: false, ..
                    } = &queued.request
                    {
                        if let Some(editor) = dock.editor.as_mut() {
                            editor.sent_text = Some(text.clone());
                        }
                    }
                    dock.data.in_flight = Some(InFlightRequest {
                        request: queued.request,
                        request_id,
                    });
                }
            } else if let Some(dock) = self.info_dock.as_deref_mut() {
                // Offline after all: try again on a later tick.
                dock.data.queue.push_front(QueuedRequest {
                    request: queued.request,
                    not_before: Some(now + BUSY_RETRY),
                });
            }
            return;
        }
    }

    // ------------------------------------------------------------ replies

    /// Every `Info*` reply; never raises a notice.
    pub(super) fn handle_info_dock_endpoint_result(
        &mut self,
        _pending: PendingEndpointRequest,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let now = Instant::now();
        let Some(dock) = self.info_dock.as_deref_mut() else {
            return (false, Vec::new());
        };
        let request = match dock.data.in_flight.take() {
            Some(in_flight) => in_flight.request,
            // A reply for a request the dock already gave up on.
            None => return (false, Vec::new()),
        };
        match result {
            Err(error) => dock_reply_error(dock, request, error, now),
            Ok(result) => dock_reply(dock, request, result, now),
        }
        let mut outcome = ClientShellInput::default();
        self.pump_info_dock(now, &mut outcome);
        (true, outcome.actions)
    }
}

impl ClientInfoDockState {
    /// Follow the editor's cursor on the next frame.
    fn ui_for_current_follow(&mut self) {
        let key = self.ui_key();
        self.ui_for(&key).follow = true;
    }
}

// ---------------------------------------------------------------- helpers

fn dock_line(dock: &ClientInfoDockState, line: usize) -> Option<&str> {
    if dock.editing() {
        return dock
            .editor
            .as_ref()
            .and_then(|editor| editor.lines.get(line))
            .map(String::as_str);
    }
    dock.data
        .notes
        .as_ref()
        .and_then(|notes| notes.text.as_deref())
        .and_then(|text| text.split('\n').nth(line))
}

fn dock_edit_at(dock: &mut ClientInfoDockState, line: usize, byte: usize, now: Instant) {
    if dock.editing() {
        if let Some(editor) = dock.editor.as_mut() {
            editor.cursor = (line, byte);
            editor.clamp_cursor();
        }
        dock.bump_editor();
    } else {
        dock.start_edit(Some((line, byte)), now);
    }
    dock.ui_for_current_follow();
}

fn dock_scroll(dock: &mut ClientInfoDockState, key: &str, down: bool, lines: usize) {
    let ui = dock.ui_for(key);
    let scroll = &mut ui.notes_scroll;
    *scroll = if down {
        scroll.saturating_add(lines)
    } else {
        scroll.saturating_sub(lines)
    };
    ui.follow = false;
}

/// A task marker in the read view: a compare-and-swap save of the toggled
/// line against the revision the row was built from.
fn dock_tick_task(dock: &mut ClientInfoDockState, line: usize, revision: &str, now: Instant) {
    let Some(notes) = dock.data.notes.as_ref() else {
        return;
    };
    if notes.revision != revision {
        dock.queue(InfoRequest::NotesGet, None);
        dock.set_flash("notes changed, tick again", now);
        return;
    }
    let Some(text) = notes
        .text
        .as_deref()
        .and_then(|text| model::toggle_task_line(text, line))
    else {
        return;
    };
    let request = InfoRequest::NotesSet {
        key: notes.key.clone(),
        text,
        base_revision: (notes.exists && notes.revision != "none").then(|| notes.revision.clone()),
        tick: true,
    };
    dock.queue(request, None);
}

fn dock_conflict_reload(dock: &mut ClientInfoDockState) {
    let Some(editor) = dock.editor.as_mut() else {
        return;
    };
    let Some(theirs) = editor.conflict.take() else {
        return;
    };
    let text = theirs.text.clone().unwrap_or_default();
    editor.lines = text.split('\n').map(str::to_owned).collect();
    editor.base_text = text;
    editor.base_revision =
        (theirs.exists && theirs.revision != "none").then(|| theirs.revision.clone());
    editor.dirty = false;
    editor.clamp_cursor();
    dock.data.notes = Some(theirs);
    dock.bump_data();
    dock.bump_editor();
}

fn dock_conflict_keep_mine(dock: &mut ClientInfoDockState) {
    let Some(editor) = dock.editor.as_mut() else {
        return;
    };
    let Some(theirs) = editor.conflict.take() else {
        return;
    };
    let their_text = theirs.text.clone().unwrap_or_default();
    let (merged, _) = model::keep_mine_merge(&editor.base_text, &editor.lines, &their_text);
    editor.lines = merged;
    editor.base_text = their_text;
    editor.base_revision =
        (theirs.exists && theirs.revision != "none").then(|| theirs.revision.clone());
    editor.dirty = true;
    editor.save_pending = false;
    editor.clamp_cursor();
    dock.bump_editor();
    dock.commit_editor(false);
}

/// The method and pending kind for `request`; `None` drops it.
fn info_request_method(
    dock: &ClientInfoDockState,
    request: &InfoRequest,
    tab_id: Option<&str>,
) -> Option<(Method, PendingEndpointKind)> {
    let tab_target = || {
        tab_id.map(|tab_id| NotesTarget {
            tab_id: Some(tab_id.to_owned()),
            ..NotesTarget::default()
        })
    };
    let key_target = |key: &str| NotesTarget {
        key: Some(key.to_owned()),
        ..NotesTarget::default()
    };
    Some(match request {
        InfoRequest::NotesGet => (
            Method::NotesGet(NotesGetParams {
                target: tab_target()?,
                known_revision: dock
                    .data
                    .notes
                    .as_ref()
                    .filter(|notes| notes.text.is_some())
                    .map(|notes| notes.revision.clone()),
            }),
            PendingEndpointKind::InfoNotesGet,
        ),
        InfoRequest::NotesSet {
            key,
            text,
            base_revision,
            ..
        } => (
            Method::NotesSet(NotesSetParams {
                target: key_target(key),
                text: text.clone(),
                base_revision: base_revision.clone(),
                author: NotesAuthor::User,
            }),
            PendingEndpointKind::InfoNotesWrite,
        ),
    })
}

fn dock_reply_error(
    dock: &mut ClientInfoDockState,
    request: InfoRequest,
    error: ClientShellEndpointError,
    now: Instant,
) {
    let code = error.code.as_deref().unwrap_or("invalid_response");
    let save = matches!(request, InfoRequest::NotesSet { tick: false, .. });
    match code {
        // Another command held the server's slot: same request, shortly.
        "endpoint_busy" => {
            dock.data.queue.push_front(QueuedRequest {
                request,
                not_before: Some(now + BUSY_RETRY),
            });
            return;
        }
        "notes_disabled" => {
            dock.data.disabled = true;
            dock.bump_data();
        }
        "not_found" => match request {
            InfoRequest::NotesGet => {
                dock.data.notes = None;
                dock.data.notes_missing = true;
                dock.bump_data();
            }
            _ => dock.set_flash("not found", now),
        },
        "too_large" => {
            if matches!(request, InfoRequest::NotesGet) {
                dock.data.notes_error =
                    Some("notes too large to show (the file is under <config dir>/notes)".into());
                dock.bump_data();
            } else {
                dock.set_flash("notes too large", now);
            }
        }
        // The request is gone (timeout, cancelled, server away): a save
        // stays dirty and re-arms on the idle timer.
        "endpoint_timeout" | "endpoint_cancelled" | "server_unavailable" => {}
        _ => dock.set_flash(format!("not applied: {}", error.message), now),
    }
    if save {
        if let Some(editor) = dock.editor.as_mut() {
            editor.save_pending = false;
            editor.sent_text = None;
            editor.leaving = false;
        }
        dock.bump_editor();
    }
}

fn dock_reply(
    dock: &mut ClientInfoDockState,
    request: InfoRequest,
    result: ResponseResult,
    now: Instant,
) {
    match (request, result) {
        (InfoRequest::NotesGet, ResponseResult::NotesGet { notes }) => {
            dock.data.notes_missing = false;
            dock.data.notes_error = None;
            if notes.unchanged {
                if let Some(current) = dock.data.notes.as_mut() {
                    if current.key == notes.key && current.revision == notes.revision {
                        return;
                    }
                }
                // Unchanged against a revision we no longer hold: ask again.
                dock.data.notes = None;
                dock.queue(InfoRequest::NotesGet, None);
                return;
            }
            let mut notes = notes;
            notes.text = Some(model::clean(notes.text.as_deref().unwrap_or_default()));
            let key_changed = dock.data.key() != Some(notes.key.as_str());
            // A conflict that came without their text gets it now.
            if let Some(conflict) = dock
                .editor
                .as_mut()
                .filter(|editor| editor.key == notes.key)
                .and_then(|editor| editor.conflict.as_mut())
                .filter(|conflict| conflict.text.is_none())
            {
                *conflict = notes.clone();
                dock.bump_editor();
            }
            dock.data.notes = Some(notes);
            if key_changed {
                dock.bump_ui();
            }
            dock.bump_data();
        }
        (
            InfoRequest::NotesSet {
                key: notes_key,
                text,
                tick,
                ..
            },
            ResponseResult::NotesWrite { write },
        ) => {
            let mut notes = write.notes;
            match write.outcome {
                NotesWriteOutcome::Written | NotesWriteOutcome::Unchanged => {
                    if notes.text.is_none() {
                        notes.text = Some(text.clone());
                    } else {
                        notes.text = Some(model::clean(notes.text.as_deref().unwrap_or_default()));
                    }
                    if dock
                        .data
                        .notes
                        .as_ref()
                        .is_none_or(|shown| shown.key == notes.key)
                    {
                        dock.data.notes = Some(notes.clone());
                        dock.data.notes_missing = false;
                        dock.bump_data();
                    }
                    if !tick {
                        if let Some(editor) = dock.editor.as_mut().filter(|e| e.key == notes_key) {
                            editor.base_revision = Some(notes.revision.clone());
                            editor.base_text = text.clone();
                            editor.save_pending = false;
                            editor.sent_text = None;
                            editor.dirty = editor.text() != text;
                            if editor.leaving && !editor.dirty {
                                dock.editor = None;
                            }
                            dock.bump_editor();
                        }
                    }
                }
                NotesWriteOutcome::Conflict | NotesWriteOutcome::Unknown => {
                    if let Some(text) = notes.text.as_deref() {
                        notes.text = Some(model::clean(text));
                    }
                    if tick {
                        dock.set_flash("notes changed, tick again", now);
                        dock.queue(InfoRequest::NotesGet, None);
                        return;
                    }
                    let need_text = notes.text.is_none();
                    let Some(editor) = dock.editor.as_mut().filter(|e| e.key == notes_key) else {
                        dock.queue(InfoRequest::NotesGet, None);
                        return;
                    };
                    editor.save_pending = false;
                    editor.sent_text = None;
                    editor.leaving = false;
                    editor.conflict = Some(notes);
                    dock.bump_editor();
                    if need_text {
                        // Without their text a merge has nothing to keep.
                        dock.queue(InfoRequest::NotesGet, None);
                    }
                }
            }
        }
        (request, _) => {
            tracing::debug!(?request, "info dock: unexpected reply shape");
            dock.set_flash("unexpected reply from the server", now);
        }
    }
}

impl PendingEndpointKind {
    /// A request of the info dock (its replies never raise a notice).
    pub(super) fn is_info_dock(&self) -> bool {
        matches!(self, Self::InfoNotesGet | Self::InfoNotesWrite)
    }
}
