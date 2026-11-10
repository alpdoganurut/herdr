//! Fork (perf watch): the opt-in `[ui] perf_readout` corner readout, e.g.
//! `58 fps · 4.1 ms · srv 3.2 ms`.
//!
//! Client presentation only. The client part comes from this process's
//! counters (src/perf `client::*`): frames presented per second and the
//! slowest frame's encode + write in the last second. The server part comes
//! from `server.perf_summary`, polled at most once a second and only from a
//! server that advertises it: the average full render over the last poll
//! interval, and the server's stall count once a loop has stalled. Off: the
//! tick returns at once, nothing is polled, composed or drawn.
//!
//! The text is rebuilt once a second in the tick; compose only copies it into
//! the bottom-right corner. Surface patches landing under the readout fall
//! back to a full compose (see [`perf_readout_blocks_patch`]) so it is not
//! overwritten.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::Style;

use super::*;
use crate::api::schema::perf::ServerPerfSummaryInfo;
use crate::api::schema::{Method, ServerPerfSummaryParams};

/// How often the readout refreshes and polls the server.
pub(super) const PERF_READOUT_INTERVAL: Duration = Duration::from_secs(1);

/// The server numbers one poll needs for the next one's deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ServerSample {
    pid: u32,
    started_unix_ms: u64,
    generation: u64,
    full_renders: u64,
    full_render_total_us: u64,
}

impl ServerSample {
    fn of(perf: &ServerPerfSummaryInfo) -> Self {
        Self {
            pid: perf.pid,
            started_unix_ms: perf.started_unix_ms,
            generation: perf.generation,
            full_renders: perf.full_renders,
            full_render_total_us: perf.full_render_total_us,
        }
    }

    /// The same server process and counter generation.
    fn same_run(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.started_unix_ms == other.started_unix_ms
            && self.generation == other.generation
    }
}

#[derive(Debug, Default)]
pub(crate) struct PerfReadoutState {
    last_tick: Option<Instant>,
    last_frames: u64,
    /// `fps · ms`, rebuilt each tick.
    client_text: String,
    /// ` · srv N ms` / ` · N stalls`, rebuilt on each answer.
    server_text: String,
    server_sample: Option<ServerSample>,
    /// ` client_text server_text `, rebuilt only when either changes, so
    /// compose formats nothing.
    padded: String,
    /// `padded`'s width in cells.
    padded_width: u16,
    loading: bool,
    /// Polls sent (tests read it).
    pub(super) polls: u64,
    /// Where the readout was last drawn.
    pub(super) drawn: Option<Rect>,
}

impl PerfReadoutState {
    fn rebuild_padded(&mut self) {
        self.padded.clear();
        if !self.client_text.is_empty() {
            self.padded.push(' ');
            self.padded.push_str(&self.client_text);
            self.padded.push_str(&self.server_text);
            self.padded.push(' ');
        }
        self.padded_width = u16::try_from(self.padded.chars().count()).unwrap_or(u16::MAX);
    }

    /// A new connection (or a reset projection): the poll in flight never
    /// answers and the old server's numbers do not apply.
    pub(super) fn forget_server(&mut self) {
        self.loading = false;
        self.server_sample = None;
        self.server_text.clear();
        self.rebuild_padded();
    }
}

/// The client half of the text for `frames` presented over `elapsed`, the
/// slowest taking `max_ns`.
pub(super) fn client_text(frames: u64, elapsed: Duration, max_ns: u64) -> String {
    let secs = elapsed.as_secs_f64();
    let fps = if secs > 0.0 {
        (frames as f64 / secs).round() as u64
    } else {
        0
    };
    format!("{fps} fps · {:.1} ms", max_ns as f64 / 1_000_000.0)
}

/// The server half from two answers of the same run (`previous`) or none.
fn server_text(previous: Option<ServerSample>, perf: &ServerPerfSummaryInfo) -> String {
    let mut text = String::new();
    let current = ServerSample::of(perf);
    if let Some(previous) = previous.filter(|previous| previous.same_run(&current)) {
        let renders = current.full_renders.saturating_sub(previous.full_renders);
        if renders > 0 {
            let us = current
                .full_render_total_us
                .saturating_sub(previous.full_render_total_us);
            text.push_str(&format!(
                " · srv {:.1} ms",
                us as f64 / renders as f64 / 1000.0
            ));
        }
    }
    let stalls: u64 = perf.loops.iter().map(|lp| lp.stalls).sum();
    if stalls > 0 {
        text.push_str(&format!(
            " · {stalls} stall{}",
            if stalls == 1 { "" } else { "s" }
        ));
    }
    text
}

impl ClientShellState {
    /// The tick: once a second while `[ui] perf_readout` is on, rebuild the
    /// text and poll the server. Off: no work (and a readout left on screen
    /// by a live config change is cleared once).
    pub(crate) fn tick_perf_readout(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        if !self.config.perf_readout {
            if self.perf_readout.last_tick.is_some() || self.perf_readout.drawn.is_some() {
                self.perf_readout = PerfReadoutState::default();
                outcome.repaint = true;
            }
            return;
        }
        let frames = crate::perf::client::FRAMES_PRESENTED.get();
        let Some(last) = self.perf_readout.last_tick else {
            // First tick: start the window; the readout shows from the next.
            self.perf_readout.last_tick = Some(now);
            self.perf_readout.last_frames = frames;
            let _ = crate::perf::client::FRAME_NS_MAX.take();
            self.queue_perf_summary(outcome);
            return;
        };
        let elapsed = now.saturating_duration_since(last);
        if elapsed < PERF_READOUT_INTERVAL {
            return;
        }
        let text = client_text(
            frames.saturating_sub(self.perf_readout.last_frames),
            elapsed,
            crate::perf::client::FRAME_NS_MAX.take(),
        );
        self.perf_readout.last_tick = Some(now);
        self.perf_readout.last_frames = frames;
        if text != self.perf_readout.client_text {
            self.perf_readout.client_text = text;
            self.perf_readout.rebuild_padded();
            outcome.repaint = true;
        }
        self.queue_perf_summary(outcome);
    }

    /// `server.perf_summary`, silently skipped while one is in flight or on
    /// a server that does not advertise it (an older server, or one whose
    /// method list is unknown) or is offline.
    fn queue_perf_summary(&mut self, outcome: &mut ClientShellInput) {
        if self.perf_readout.loading || !self.endpoint_is_online(&self.active_endpoint_id) {
            return;
        }
        let advertised = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.methods.as_ref())
            .is_some_and(|methods| methods.contains(crate::api::schema::perf::METHOD));
        if !advertised {
            return;
        }
        let method = Method::ServerPerfSummary(ServerPerfSummaryParams::default());
        if self.push_endpoint_method_with_kind(method, PendingEndpointKind::PerfSummary, outcome) {
            self.perf_readout.loading = true;
            self.perf_readout.polls += 1;
        }
    }

    /// A `server.perf_summary` answer: never a notice; an error or another
    /// result only ends the poll.
    pub(super) fn complete_perf_readout(
        &mut self,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        self.perf_readout.loading = false;
        let Ok(crate::api::schema::ResponseResult::ServerPerfSummary { perf }) = result else {
            return (false, Vec::new());
        };
        if !self.config.perf_readout {
            return (false, Vec::new());
        }
        let text = server_text(self.perf_readout.server_sample, &perf);
        self.perf_readout.server_sample = Some(ServerSample::of(&perf));
        let changed = text != self.perf_readout.server_text;
        self.perf_readout.server_text = text;
        self.perf_readout.rebuild_padded();
        (
            changed && !self.perf_readout.client_text.is_empty(),
            Vec::new(),
        )
    }

    /// Writes the readout straight into the bottom-right cells of `frame`;
    /// returns where (None while off or before the first second). No buffer
    /// round trip and no formatting: the text is built in the tick.
    pub(super) fn paint_perf_readout(
        &mut self,
        frame: &mut crate::protocol::FrameData,
    ) -> Option<Rect> {
        self.perf_readout.drawn = None;
        let state = &self.perf_readout;
        let width = state.padded_width;
        if !self.config.perf_readout
            || state.padded.is_empty()
            || frame.width < width
            || frame.height == 0
        {
            return None;
        }
        let rect = Rect::new(frame.width - width, frame.height - 1, width, 1);
        let palette = &self.config.palette;
        let mut styled = ratatui::buffer::Cell::default();
        styled.set_style(Style::default().fg(palette.overlay0).bg(palette.panel_bg));
        let style = crate::protocol::CellData::from_ratatui_cell(&styled);
        let row = usize::from(rect.y) * usize::from(frame.width);
        for (offset, ch) in state.padded.chars().enumerate() {
            let cell = frame.cells.get_mut(row + usize::from(rect.x) + offset)?;
            cell.symbol.clear();
            cell.symbol.push(ch);
            cell.fg = style.fg;
            cell.bg = style.bg;
            cell.modifier = style.modifier;
            cell.skip = false;
            cell.hyperlink = None;
        }
        self.perf_readout.drawn = Some(rect);
        Some(rect)
    }
}

/// A patch row lands on the drawn readout, which it would overwrite until the
/// next full compose.
pub(super) fn perf_readout_blocks_patch(
    state: &ClientShellState,
    patch: &crate::protocol::PaneSurfacePatch,
) -> bool {
    let Some(readout) = state.perf_readout.drawn else {
        return false;
    };
    let (cols, rows) = state.last_composed_size.unwrap_or_default();
    let area = state.layout(cols, rows).pane_surface;
    patch.rows.iter().any(|row| {
        Rect::new(
            area.x.saturating_add(row.x),
            area.y.saturating_add(row.y),
            u16::try_from(row.cells.len()).unwrap_or(u16::MAX),
            1,
        )
        .intersects(readout)
    })
}
