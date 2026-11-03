//! Fork: deadline-based frame pacing for the server's render loop.
//!
//! Upstream counted the render interval from the last render attempt
//! (`last + 16 ms`). The loop's wake and the render itself add ~2 ms per cycle,
//! so a scrolling pane presented at ~55 fps, drifting against the display
//! (60 Hz: a repeated frame every ~10; 120 Hz: 2/3-refresh alternation).
//!
//! A [`FramePacer`] keeps a fixed grid instead: each frame's deadline is the
//! previous deadline plus the interval, so a late wake does not push later
//! frames back. The first change after idle still renders at once (and anchors
//! a new grid), a frame that is a full interval or more behind re-anchors
//! instead of bursting to catch up, and an out-of-band render (the interactive
//! echo path) leaves the grid alone. A frame may start up to
//! [`FramePacer::tolerance`] before its grid instant, so a wake that lands just
//! short of it does not wait for another timer tick. Time is injected, so the
//! rules are pure.

use std::time::{Duration, Instant};

/// The frame interval for `fps` frames per second (60 → 16.667 ms).
pub(crate) fn render_interval(fps: u16) -> Duration {
    Duration::from_secs(1) / u32::from(fps.max(1))
}

/// Below this remainder the loop sleeps straight to the deadline.
const EARLY_WAKE_FLOOR: Duration = Duration::from_millis(1);
/// The most a frame may start before its grid instant.
const MAX_DUE_TOLERANCE: Duration = Duration::from_millis(1);

/// When the loop should wake for a frame due at `deadline`.
///
/// macOS stretches a timed wait by up to a quarter of its length (timer
/// coalescing: a 16.7 ms wait wakes 0-4.2 ms late), which would put that
/// jitter on every frame. Waking a quarter of the remaining time early keeps
/// the wake at or before the deadline; the loop then waits again for the
/// shorter remainder, so a frame lands within about 1 ms of its deadline after
/// two or three short waits. Elsewhere timer slack is tens of microseconds and
/// this costs one extra wait per frame. Only a pending frame wakes the loop
/// this way: about three extra loop passes per frame while output changes
/// (~180/s at 60 fps), none while idle, independent of pane count.
pub(crate) fn wake_at(now: Instant, deadline: Instant) -> Instant {
    let remaining = deadline.saturating_duration_since(now);
    if remaining <= EARLY_WAKE_FLOOR {
        deadline
    } else {
        deadline - remaining / 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FramePacer {
    interval: Duration,
    /// The earliest instant of the next paced frame; `None` before the first.
    next_deadline: Option<Instant>,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::new(render_interval(crate::config::DEFAULT_RENDER_FPS))
    }
}

impl FramePacer {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            next_deadline: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn interval(&self) -> Duration {
        self.interval
    }

    /// Changes the interval; the next frame is due at once and anchors a new grid.
    pub(crate) fn set_interval(&mut self, interval: Duration) {
        if interval != self.interval {
            self.interval = interval;
            self.next_deadline = None;
        }
    }

    /// How early a frame may start before its grid instant: 1 ms, or an
    /// eighth of the interval when that is shorter.
    fn tolerance(&self) -> Duration {
        MAX_DUE_TOLERANCE.min(self.interval / 8)
    }

    /// Whether a paced frame may render at `now`.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.due_at().is_none_or(|due| now >= due)
    }

    /// The earliest instant the next paced frame may render, if a frame has
    /// rendered before (it may already be past).
    pub(crate) fn due_at(&self) -> Option<Instant> {
        self.next_deadline
            .map(|deadline| deadline - self.tolerance())
    }

    /// The grid instant of the next paced frame.
    #[cfg(test)]
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.next_deadline
    }

    /// Records a render at `now` and schedules the next deadline.
    pub(crate) fn record(&mut self, now: Instant) {
        self.next_deadline = match self.next_deadline {
            // First frame, or idle / behind by a full interval or more:
            // anchor a new grid at this frame (no catch-up burst).
            None => Some(now + self.interval),
            Some(deadline) if now >= deadline + self.interval => Some(now + self.interval),
            // On time or woken late within the interval: stay on the grid.
            Some(deadline) if now + self.tolerance() >= deadline => Some(deadline + self.interval),
            // Out-of-band render before the deadline: keep the grid.
            Some(deadline) => Some(deadline),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn pacer() -> FramePacer {
        FramePacer::new(render_interval(60))
    }

    #[test]
    fn interval_matches_the_display_rate() {
        assert_eq!(render_interval(60), Duration::from_nanos(16_666_666));
        assert_eq!(render_interval(120), Duration::from_nanos(8_333_333));
        assert_eq!(render_interval(0), Duration::from_secs(1));
    }

    #[test]
    fn first_frame_renders_immediately() {
        let mut pacer = pacer();
        let t0 = Instant::now();
        assert!(pacer.is_due(t0));
        pacer.record(t0);
        assert_eq!(pacer.next_deadline(), Some(t0 + pacer.interval()));
        assert!(!pacer.is_due(t0 + 15 * MS));
        assert!(pacer.is_due(t0 + pacer.interval() - MS));
        assert_eq!(pacer.due_at(), Some(t0 + pacer.interval() - MS));
    }

    #[test]
    fn late_wakes_stay_on_the_grid() {
        let mut pacer = pacer();
        let interval = pacer.interval();
        let t0 = Instant::now();
        pacer.record(t0);
        // Every wake is 2 ms late; the deadlines do not drift.
        for frame in 1..=100u32 {
            let deadline = t0 + interval * frame;
            assert_eq!(pacer.next_deadline(), Some(deadline));
            let wake = deadline + 2 * MS;
            assert!(pacer.is_due(wake));
            pacer.record(wake);
        }
        assert_eq!(pacer.next_deadline(), Some(t0 + interval * 101));
    }

    #[test]
    fn first_change_after_idle_renders_at_once_and_reanchors() {
        let mut pacer = pacer();
        let t0 = Instant::now();
        pacer.record(t0);
        let after_idle = t0 + Duration::from_secs(3);
        assert!(pacer.is_due(after_idle));
        pacer.record(after_idle);
        assert_eq!(pacer.next_deadline(), Some(after_idle + pacer.interval()));
    }

    #[test]
    fn a_wake_just_short_of_the_grid_renders_on_it() {
        let mut pacer = pacer();
        let interval = pacer.interval();
        let t0 = Instant::now();
        pacer.record(t0);
        let early = t0 + interval - Duration::from_micros(600);
        assert!(pacer.is_due(early));
        pacer.record(early);
        assert_eq!(pacer.next_deadline(), Some(t0 + interval * 2));
        // The tolerance shrinks with short intervals.
        let mut fast = FramePacer::new(render_interval(240));
        fast.record(t0);
        assert!(!fast.is_due(t0 + render_interval(240) - MS));
    }

    #[test]
    fn far_behind_reanchors_instead_of_bursting() {
        let mut pacer = pacer();
        let interval = pacer.interval();
        let t0 = Instant::now();
        pacer.record(t0);
        // A 40 ms render: the next frame is more than a full interval behind its deadline.
        let slow = t0 + interval + 40 * MS;
        pacer.record(slow);
        assert_eq!(pacer.next_deadline(), Some(slow + interval));
        assert!(!pacer.is_due(slow + MS));
    }

    #[test]
    fn out_of_band_render_keeps_the_grid() {
        let mut pacer = pacer();
        let t0 = Instant::now();
        pacer.record(t0);
        let deadline = t0 + pacer.interval();
        pacer.record(t0 + 5 * MS);
        assert_eq!(pacer.next_deadline(), Some(deadline));
        assert!(!pacer.is_due(t0 + 6 * MS));
        assert!(pacer.is_due(deadline));
    }

    #[test]
    fn rate_never_exceeds_the_interval_under_constant_demand() {
        let mut pacer = pacer();
        let interval = pacer.interval();
        let t0 = Instant::now();
        let mut frames = Vec::new();
        // Poll every 0.5 ms for ten seconds with work always pending.
        for step in 0..20_000u32 {
            let now = t0 + Duration::from_micros(500) * step;
            if pacer.is_due(now) {
                pacer.record(now);
                frames.push(now);
            }
        }
        // 600 frames in 10 s (the last grid instant, 1 ms early, may land inside).
        assert!((600..=601).contains(&frames.len()), "{}", frames.len());
        // No two frames closer than the interval less twice the tolerance.
        let min_gap = frames
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .min()
            .unwrap_or(interval);
        assert!(min_gap >= interval - 2 * MS, "{min_gap:?}");
    }

    #[test]
    fn wake_at_leaves_room_for_timer_slack() {
        let now = Instant::now();
        let deadline = now + 16 * MS;
        assert_eq!(wake_at(now, deadline), deadline - 4 * MS);
        // A wait stretched by its own quarter still wakes by the deadline.
        let wake = wake_at(now, deadline);
        assert!(wake + (wake - now) / 4 <= deadline);
        // Short remainders and past deadlines go straight to the deadline.
        assert_eq!(wake_at(deadline - MS, deadline), deadline);
        assert_eq!(wake_at(deadline + MS, deadline), deadline);
    }

    #[test]
    fn render_fps_config_defaults_to_60_and_clamps_with_a_diagnostic() {
        let mut ui = crate::config::Config::default().ui;
        assert_eq!(ui.effective_render_fps(), 60);
        assert_eq!(ui.render_fps_diagnostic(), None);
        ui.render_fps = 120;
        assert_eq!(ui.effective_render_fps(), 120);
        assert_eq!(ui.render_fps_diagnostic(), None);
        ui.render_fps = 1000;
        assert_eq!(ui.effective_render_fps(), 240);
        assert!(ui
            .render_fps_diagnostic()
            .is_some_and(|diagnostic| diagnostic.contains("ui.render_fps")));
        let parsed: crate::config::Config =
            toml::from_str("[ui]\nrender_fps = 120\n").expect("render_fps parses");
        assert_eq!(parsed.ui.effective_render_fps(), 120);
    }

    #[test]
    fn interval_change_reanchors() {
        let mut pacer = pacer();
        let t0 = Instant::now();
        pacer.record(t0);
        pacer.set_interval(render_interval(120));
        assert!(pacer.is_due(t0 + MS));
        pacer.record(t0 + MS);
        assert_eq!(pacer.next_deadline(), Some(t0 + MS + render_interval(120)));
        // Setting the same interval keeps the grid.
        pacer.set_interval(render_interval(120));
        assert_eq!(pacer.next_deadline(), Some(t0 + MS + render_interval(120)));
    }
}
