//! The `tabs` sidebar layout's breathing agent glyph: on every row whose tab
//! is Working, the right-aligned agent glyph fades smoothly between its
//! normal color and a dim one (a cosine ease, `PERIOD` per full cycle).
//!
//! Colors are interpolated in RGB. A color that is not RGB is mapped through
//! the ANSI palette's standard values, and the terminal's default background
//! through the host background when it is known; when a color cannot be
//! resolved at all, the glyph toggles between the two colors at the half
//! cycle instead. No terminal blink attribute is used.
//!
//! Redraws: while a composed frame drew at least one breathing glyph
//! (`ShellHitMap::breathing`), the client loop's timer asks for a frame every
//! `FRAME` (`next_breathe_deadline`, `tick_breathing`); at rest nothing is
//! scheduled. Only the sidebar changes, which the retained pane-surface fast
//! path never patches.

use ratatui::style::Color;

use super::*;

/// One full breath: normal, dim, normal.
pub(super) const PERIOD: std::time::Duration = std::time::Duration::from_secs(2);
/// Frame interval while something breathes (~10 fps).
pub(super) const FRAME: std::time::Duration = std::time::Duration::from_millis(100);
/// How far the dim end moves from the normal color toward the row
/// background (it keeps about a third of the glyph's contrast).
const DIM_TOWARD_BACKGROUND: f32 = 0.65;

type Rgb = (u8, u8, u8);

/// A color's RGB value; `Reset` resolves to `reset` (the terminal default).
fn rgb(color: Color, reset: Option<Rgb>) -> Option<Rgb> {
    Some(match color {
        Color::Reset => return reset,
        Color::Black => (0, 0, 0),
        Color::Red => (128, 0, 0),
        Color::Green => (0, 128, 0),
        Color::Yellow => (128, 128, 0),
        Color::Blue => (0, 0, 128),
        Color::Magenta => (128, 0, 128),
        Color::Cyan => (0, 128, 128),
        Color::Gray => (192, 192, 192),
        Color::DarkGray => (128, 128, 128),
        Color::LightRed => (255, 0, 0),
        Color::LightGreen => (0, 255, 0),
        Color::LightYellow => (255, 255, 0),
        Color::LightBlue => (0, 0, 255),
        Color::LightMagenta => (255, 0, 255),
        Color::LightCyan => (0, 255, 255),
        Color::White => (255, 255, 255),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(index) => indexed_rgb(index)?,
    })
}

/// The xterm 256-color cube and gray ramp; the 16 system colors map like
/// their named forms.
fn indexed_rgb(index: u8) -> Option<Rgb> {
    const SYSTEM: [Color; 16] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    match index {
        0..=15 => rgb(SYSTEM[usize::from(index)], None),
        16..=231 => {
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            let value = index - 16;
            Some((level(value / 36), level((value / 6) % 6), level(value % 6)))
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            Some((gray, gray, gray))
        }
    }
}

fn mix(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let channel = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (
        channel(from.0, to.0),
        channel(from.1, to.1),
        channel(from.2, to.2),
    )
}

/// The breath's position in `[0, 1)` at `now`, counted from `epoch`.
pub(super) fn phase_at(epoch: std::time::Instant, now: std::time::Instant) -> f32 {
    let elapsed = now.saturating_duration_since(epoch).as_secs_f64();
    (elapsed.rem_euclid(PERIOD.as_secs_f64()) / PERIOD.as_secs_f64()) as f32
}

/// How dim the glyph is at `phase`: 0 at the start and end of a breath, 1
/// half way, eased with a cosine.
pub(super) fn dimness(phase: f32) -> f32 {
    (1.0 - (std::f32::consts::TAU * phase).cos()) / 2.0
}

/// The glyph's color at `phase`, fading from `normal` toward `background`.
/// `reset` is the RGB the terminal's default background resolves to.
pub(super) fn glyph_color(
    normal: Color,
    background: Color,
    reset: Option<Rgb>,
    phase: f32,
) -> Color {
    let dim = dimness(phase);
    // At the top of the breath the glyph keeps its color as configured
    // (a named color stays named).
    if dim < 1.0 / 512.0 {
        return normal;
    }
    match (rgb(normal, None), rgb(background, reset)) {
        (Some(from), Some(bg)) => {
            let target = mix(from, bg, DIM_TOWARD_BACKGROUND);
            let (r, g, b) = mix(from, target, dim);
            Color::Rgb(r, g, b)
        }
        // Nothing to interpolate: a two-step toggle to the dark gray.
        _ if dim >= 0.5 => Color::DarkGray,
        _ => normal,
    }
}

impl ClientShellState {
    /// The breath's phase for the next compose (the test clock when set).
    pub(super) fn breathe_phase(&self) -> f32 {
        let now = self.breathe_clock.unwrap_or_else(std::time::Instant::now);
        phase_at(self.breathe_epoch, now)
    }

    /// The terminal default background's RGB, when the host reported it.
    pub(super) fn breathe_reset_rgb(&self) -> Option<Rgb> {
        self.host_background
            .map(|color| (color.r, color.g, color.b))
    }

    /// When the next breathing frame is due; `None` at rest.
    pub(crate) fn next_breathe_deadline(&self) -> Option<std::time::Instant> {
        if !self.hits.breathing {
            return None;
        }
        Some(
            self.last_composed_at
                .map_or_else(std::time::Instant::now, |at| at + FRAME),
        )
    }

    /// Whether a breathing frame is due at `now` (the caller repaints).
    pub(crate) fn tick_breathing(&self, now: std::time::Instant) -> bool {
        self.next_breathe_deadline()
            .is_some_and(|deadline| now >= deadline)
    }
}
