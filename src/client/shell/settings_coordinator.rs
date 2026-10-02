//! The settings overlay's `coordinator` section (fork): the coordinator's
//! switches and facts, from `coordinator.get`.
//!
//! Rows act on Enter or a click: `coordinator: on|off`
//! (`coordinator.set_enabled`), `model: …` and the two wake caps open a
//! picker (`coordinator.set_model`, `coordinator.set_wake_caps` with both
//! caps), `notifications: on|off` (`coordinator.set_notify`), under the
//! `spaces` layout a hint row that switches `ui.sidebar_layout` to `tabs`
//! (the pinned row needs it; nothing switches on its own), `open dashboard`
//! (`coordinator.open_dashboard`), `wake now` (`coordinator.wake`) and
//! `restart coordinator` (`coordinator.start`). Every setter goes to the
//! active server, which writes its own config; the reply refreshes the
//! section. Below the rows: state, coordinator dir, dashboard URL (or why
//! there is none), managed count, wakes today, last wake and the session.
//! The dashboard port is shown, not edited (`[coordinator] dashboard_port`).
//!
//! Like `coordinator.rs` this holds no reference to `ClientShellState`: the
//! overlay keeps a `ClientCoordinatorSettings`, passes its cursor in, and
//! turns the returned `CoordinatorEffect` into a method, a config write or
//! a toast.

use super::coordinator::{
    dashboard_unavailable_reason, ClientCoordinatorState, CoordinatorEffect, CoordinatorRequest,
};
use super::render::put_text;
use crate::api::schema::coordinator::{CoordinatorGetInfo, CoordinatorStateInfo};
use crate::app::state::Palette;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

/// The model picker: Claude's default first.
pub(crate) const MODEL_CHOICES: &[Option<&str>] = &[None, Some("opus"), Some("sonnet")];
/// The wakes-per-hour picker.
pub(crate) const CAP_HOUR_CHOICES: &[u32] = &[4, 8, 12, 20, 30];
/// The wakes-per-day picker.
pub(crate) const CAP_DAY_CHOICES: &[u32] = &[20, 40, 80, 120, 200];

/// One row of the section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoordinatorSettingsRow {
    Enabled,
    Model,
    CapHour,
    CapDay,
    Notify,
    /// Only under the `spaces` layout.
    LayoutHint,
    OpenDashboard,
    WakeNow,
    Restart,
}

/// The rows in order; the layout hint only under `spaces`.
pub(crate) fn section_rows(spaces_layout: bool) -> Vec<CoordinatorSettingsRow> {
    use CoordinatorSettingsRow::*;
    let mut rows = vec![Enabled, Model, CapHour, CapDay, Notify];
    if spaces_layout {
        rows.push(LayoutHint);
    }
    rows.extend([OpenDashboard, WakeNow, Restart]);
    rows
}

/// A picker open in place of the section's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoordinatorPicker {
    Model(Vec<Option<String>>),
    CapHour(Vec<u32>),
    CapDay(Vec<u32>),
}

impl CoordinatorPicker {
    fn len(&self) -> usize {
        match self {
            Self::Model(choices) => choices.len(),
            Self::CapHour(choices) | Self::CapDay(choices) => choices.len(),
        }
    }

    /// The row that opened the picker.
    fn row(&self) -> CoordinatorSettingsRow {
        match self {
            Self::Model(_) => CoordinatorSettingsRow::Model,
            Self::CapHour(_) => CoordinatorSettingsRow::CapHour,
            Self::CapDay(_) => CoordinatorSettingsRow::CapDay,
        }
    }
}

/// The model picker's rows: the configured model first when it is off the
/// list.
pub(crate) fn model_choices(configured: Option<&str>) -> Vec<Option<String>> {
    let mut choices = Vec::with_capacity(MODEL_CHOICES.len() + 1);
    if let Some(model) = configured.filter(|model| !MODEL_CHOICES.contains(&Some(*model))) {
        choices.push(Some(model.to_owned()));
    }
    choices.extend(MODEL_CHOICES.iter().map(|model| model.map(str::to_owned)));
    choices
}

/// A cap picker's rows: the configured cap first when it is off the list.
pub(crate) fn cap_choices(list: &[u32], configured: u32) -> Vec<u32> {
    let mut choices = Vec::with_capacity(list.len() + 1);
    if configured > 0 && !list.contains(&configured) {
        choices.push(configured);
    }
    choices.extend_from_slice(list);
    choices
}

/// `default` for Claude's default model, else the model.
pub(crate) fn model_label(model: Option<&str>) -> &str {
    model.unwrap_or("default")
}

/// What a settings action leaves the overlay to do besides its effect: the
/// cursor to move to when a picker opened or closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinatorSettingsOutcome {
    pub(crate) effect: CoordinatorEffect,
    pub(crate) select: Option<usize>,
}

impl CoordinatorSettingsOutcome {
    fn effect(effect: CoordinatorEffect) -> Self {
        Self {
            effect,
            select: None,
        }
    }

    fn select(select: usize) -> Self {
        Self {
            effect: CoordinatorEffect::Nothing,
            select: Some(select),
        }
    }
}

/// The section's state inside the settings overlay: its own copy of the
/// read model (the renderer sees the overlay, not the shell state).
#[derive(Debug, Default)]
pub(crate) struct ClientCoordinatorSettings {
    pub(crate) info: Option<CoordinatorGetInfo>,
    pub(crate) loading: bool,
    pub(crate) picker: Option<CoordinatorPicker>,
}

impl ClientCoordinatorSettings {
    /// Entering the section: show what the shell already knows; the shell
    /// pulls a fresh record (`ClientCoordinatorState::refresh`).
    pub(crate) fn enter(&mut self, shell: &ClientCoordinatorState) {
        self.info = shell.info.clone();
        self.picker = None;
        self.loading = true;
    }

    /// A reply reached the open section; returns the cursor clamped to the
    /// rows (none to clamp while a picker is open or without a record).
    pub(crate) fn sync(
        &mut self,
        shell: &ClientCoordinatorState,
        selected: usize,
        spaces_layout: bool,
    ) -> usize {
        self.info = shell.info.clone();
        self.loading = false;
        if self.picker.is_some() {
            return selected;
        }
        match self.row_count(spaces_layout) {
            0 => selected,
            rows => selected.min(rows - 1),
        }
    }

    /// Rows in the section: the picker's while one is open, else the
    /// switches and actions (none without a record).
    pub(crate) fn row_count(&self, spaces_layout: bool) -> usize {
        match (&self.picker, &self.info) {
            (Some(picker), _) => picker.len(),
            (None, Some(_)) => section_rows(spaces_layout).len(),
            (None, None) => 0,
        }
    }

    /// `[coordinator] notify` as the read model reports it.
    fn notify(&self) -> bool {
        self.info.as_ref().is_none_or(|info| info.notify)
    }

    /// Enter or a click on row `selected`. `remote` as for the row menu's
    /// Open dashboard.
    pub(crate) fn apply(
        &mut self,
        selected: usize,
        spaces_layout: bool,
        remote: bool,
    ) -> CoordinatorSettingsOutcome {
        let Some(info) = self.info.as_ref() else {
            return CoordinatorSettingsOutcome::effect(CoordinatorEffect::Nothing);
        };
        if let Some(picker) = self.picker.take() {
            let rows = section_rows(spaces_layout);
            let back = rows.iter().position(|row| *row == picker.row());
            let request = match picker {
                CoordinatorPicker::Model(choices) => choices
                    .get(selected)
                    .cloned()
                    .map(CoordinatorRequest::SetModel),
                // The server refuses an hourly cap above the daily one:
                // the picked value wins and the other follows it.
                CoordinatorPicker::CapHour(choices) => {
                    choices
                        .get(selected)
                        .map(|cap_hour| CoordinatorRequest::SetWakeCaps {
                            cap_hour: *cap_hour,
                            cap_day: info.wake.cap_day.max(*cap_hour),
                        })
                }
                CoordinatorPicker::CapDay(choices) => {
                    choices
                        .get(selected)
                        .map(|cap_day| CoordinatorRequest::SetWakeCaps {
                            cap_hour: info.wake.cap_hour.min(*cap_day),
                            cap_day: *cap_day,
                        })
                }
            };
            return CoordinatorSettingsOutcome {
                effect: request.map_or(CoordinatorEffect::Nothing, CoordinatorEffect::Request),
                select: back,
            };
        }
        let Some(row) = section_rows(spaces_layout).get(selected).copied() else {
            return CoordinatorSettingsOutcome::effect(CoordinatorEffect::Nothing);
        };
        match row {
            CoordinatorSettingsRow::Enabled => CoordinatorSettingsOutcome::effect(
                CoordinatorEffect::Request(CoordinatorRequest::SetEnabled(!info.enabled)),
            ),
            CoordinatorSettingsRow::Model => {
                let choices = model_choices(info.model.as_deref());
                let cursor = choices
                    .iter()
                    .position(|model| *model == info.model)
                    .unwrap_or(0);
                self.picker = Some(CoordinatorPicker::Model(choices));
                CoordinatorSettingsOutcome::select(cursor)
            }
            CoordinatorSettingsRow::CapHour => {
                let choices = cap_choices(CAP_HOUR_CHOICES, info.wake.cap_hour);
                let cursor = choices
                    .iter()
                    .position(|cap| *cap == info.wake.cap_hour)
                    .unwrap_or(0);
                self.picker = Some(CoordinatorPicker::CapHour(choices));
                CoordinatorSettingsOutcome::select(cursor)
            }
            CoordinatorSettingsRow::CapDay => {
                let choices = cap_choices(CAP_DAY_CHOICES, info.wake.cap_day);
                let cursor = choices
                    .iter()
                    .position(|cap| *cap == info.wake.cap_day)
                    .unwrap_or(0);
                self.picker = Some(CoordinatorPicker::CapDay(choices));
                CoordinatorSettingsOutcome::select(cursor)
            }
            CoordinatorSettingsRow::Notify => {
                let next = !info.notify;
                CoordinatorSettingsOutcome::effect(CoordinatorEffect::Request(
                    CoordinatorRequest::SetNotify(next),
                ))
            }
            CoordinatorSettingsRow::LayoutHint => {
                CoordinatorSettingsOutcome::effect(CoordinatorEffect::SwitchLayoutToTabs)
            }
            CoordinatorSettingsRow::OpenDashboard => {
                CoordinatorSettingsOutcome::effect(match dashboard_unavailable_reason(info) {
                    Some(reason) => CoordinatorEffect::Refused(reason),
                    None => CoordinatorEffect::Request(CoordinatorRequest::OpenDashboard {
                        open: !remote,
                    }),
                })
            }
            CoordinatorSettingsRow::WakeNow if !info.enabled => CoordinatorSettingsOutcome::effect(
                CoordinatorEffect::Refused("coordinator off".into()),
            ),
            CoordinatorSettingsRow::WakeNow => CoordinatorSettingsOutcome::effect(
                CoordinatorEffect::Request(CoordinatorRequest::Wake),
            ),
            CoordinatorSettingsRow::Restart if !info.enabled => CoordinatorSettingsOutcome::effect(
                CoordinatorEffect::Refused("coordinator off".into()),
            ),
            CoordinatorSettingsRow::Restart => CoordinatorSettingsOutcome::effect(
                CoordinatorEffect::Request(CoordinatorRequest::Start { resume: true }),
            ),
        }
    }

    /// Leave an open picker for the rows; returns the cursor on the row
    /// that opened it, or `None` when no picker was open (esc then closes
    /// the overlay as usual).
    pub(crate) fn close_picker(&mut self, spaces_layout: bool) -> Option<usize> {
        let picker = self.picker.take()?;
        section_rows(spaces_layout)
            .iter()
            .position(|row| *row == picker.row())
            .or(Some(0))
    }
}

/// A row's label.
fn row_label(
    row: CoordinatorSettingsRow,
    info: &CoordinatorGetInfo,
    notify: bool,
) -> std::borrow::Cow<'static, str> {
    let on_off = |on: bool| if on { "on" } else { "off" };
    match row {
        CoordinatorSettingsRow::Enabled => format!("coordinator: {}", on_off(info.enabled)).into(),
        CoordinatorSettingsRow::Model => {
            format!("model: {}", model_label(info.model.as_deref())).into()
        }
        CoordinatorSettingsRow::CapHour => format!("wakes per hour: {}", info.wake.cap_hour).into(),
        CoordinatorSettingsRow::CapDay => format!("wakes per day: {}", info.wake.cap_day).into(),
        CoordinatorSettingsRow::Notify => format!("notifications: {}", on_off(notify)).into(),
        CoordinatorSettingsRow::LayoutHint => {
            "pinned row needs sidebar layout: tabs — switch".into()
        }
        CoordinatorSettingsRow::OpenDashboard => "open dashboard".into(),
        CoordinatorSettingsRow::WakeNow => "wake now".into(),
        CoordinatorSettingsRow::Restart => "restart coordinator".into(),
    }
}

/// The state fact: the lifecycle state and its reason.
pub(crate) fn state_text(info: &CoordinatorGetInfo) -> String {
    let state = match info.state {
        CoordinatorStateInfo::Off => "off",
        CoordinatorStateInfo::WaitingForLock => "waiting for lock",
        CoordinatorStateInfo::Starting => "starting",
        CoordinatorStateInfo::Running => "running",
        CoordinatorStateInfo::Down => "down",
        CoordinatorStateInfo::Blocked => "blocked",
        CoordinatorStateInfo::Unavailable => "unavailable",
        CoordinatorStateInfo::Unknown => "unknown",
    };
    match info
        .down_reason
        .as_deref()
        .or(info.blocked_reason.as_deref())
    {
        Some(reason) if !reason.is_empty() => format!("{state} ({})", reason.replace('_', " ")),
        _ => state.to_owned(),
    }
}

/// The dashboard fact: the URL, or why there is none.
pub(crate) fn dashboard_text(info: &CoordinatorGetInfo) -> String {
    match (
        info.dashboard_url.as_deref(),
        info.dashboard_error.as_deref(),
    ) {
        (Some(url), _) => url.to_owned(),
        (None, Some(error)) => error.to_owned(),
        (None, None) if !info.enabled => "off".to_owned(),
        (None, None) => "not serving".to_owned(),
    }
}

/// The wakes fact: today against the cap, and capped.
pub(crate) fn wakes_text(info: &CoordinatorGetInfo) -> String {
    let wake = &info.wake;
    let mut text = format!(
        "{}/{} today · {}/{} this hour",
        wake.day, wake.cap_day, wake.hour, wake.cap_hour
    );
    if wake.capped {
        text.push_str(" · capped");
    }
    if wake.pending > 0 {
        text.push_str(&format!(" · {} pending", wake.pending));
    }
    text
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

fn draw_row(
    buffer: &mut Buffer,
    rect: Rect,
    label: &str,
    selected: bool,
    current: bool,
    palette: &Palette,
) {
    let style = if selected {
        Style::default()
            .fg(super::panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text).bg(palette.panel_bg)
    };
    buffer.set_style(rect, style);
    let marker = if selected { "▸" } else { " " };
    let current = if current { " ✓" } else { "" };
    put_text(
        buffer,
        rect.x,
        rect.y,
        rect.width,
        &format!(" {marker} {label}{current}"),
        style,
    );
}

/// One `label  value` line under the rows.
fn draw_fact(buffer: &mut Buffer, area: Rect, y: u16, label: &str, value: &str, palette: &Palette) {
    if y >= area.bottom() {
        return;
    }
    let dim = Style::default().fg(palette.overlay1).bg(palette.panel_bg);
    let text = Style::default().fg(palette.text).bg(palette.panel_bg);
    put_text(
        buffer,
        area.x,
        y,
        area.width,
        &format!("   {label:<10}"),
        dim,
    );
    let x = area.x.saturating_add(13);
    put_text(buffer, x, y, area.right().saturating_sub(x), value, text);
}

/// The section: a title, the rows (or the open picker), then the facts.
/// `hits` gets each row's rect and index.
pub(crate) fn render_coordinator_section(
    buffer: &mut Buffer,
    area: Rect,
    settings: &ClientCoordinatorSettings,
    selected: usize,
    spaces_layout: bool,
    palette: &Palette,
    hits: &mut Vec<(Rect, usize)>,
) {
    let title_style = Style::default()
        .fg(palette.text)
        .bg(palette.panel_bg)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(palette.overlay1).bg(palette.panel_bg);
    if let Some(picker) = settings.picker.as_ref() {
        let info = settings.info.as_ref();
        let (title, help, rows): (&str, &str, Vec<(String, bool)>) = match picker {
            CoordinatorPicker::Model(choices) => (
                "coordinator model",
                "applies at the coordinator's next launch; ↵ keeps it, esc goes back",
                choices
                    .iter()
                    .map(|model| {
                        (
                            model_label(model.as_deref()).to_owned(),
                            info.is_some_and(|info| info.model == *model),
                        )
                    })
                    .collect(),
            ),
            CoordinatorPicker::CapHour(choices) => (
                "wakes per hour",
                "the most wake-ups in any hour; ↵ keeps it, esc goes back",
                choices
                    .iter()
                    .map(|cap| {
                        (
                            cap.to_string(),
                            info.is_some_and(|info| info.wake.cap_hour == *cap),
                        )
                    })
                    .collect(),
            ),
            CoordinatorPicker::CapDay(choices) => (
                "wakes per day",
                "the most wake-ups in a day; ↵ keeps it, esc goes back",
                choices
                    .iter()
                    .map(|cap| {
                        (
                            cap.to_string(),
                            info.is_some_and(|info| info.wake.cap_day == *cap),
                        )
                    })
                    .collect(),
            ),
        };
        put_text(buffer, area.x, area.y, area.width, title, title_style);
        put_text(
            buffer,
            area.x,
            area.y.saturating_add(1),
            area.width,
            help,
            dim,
        );
        for (index, (label, current)) in rows.iter().enumerate() {
            let y = area.y.saturating_add(3).saturating_add(index as u16);
            if y >= area.bottom() {
                break;
            }
            let rect = Rect::new(area.x, y, area.width, 1);
            draw_row(buffer, rect, label, index == selected, *current, palette);
            hits.push((rect, index));
        }
        return;
    }

    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        "coordinator",
        title_style,
    );
    put_text(
        buffer,
        area.x,
        area.y.saturating_add(1),
        area.width,
        "an agent that watches your managed agents; ↵ toggles, picks, acts",
        dim,
    );
    let Some(info) = settings.info.as_ref() else {
        let message = if settings.loading {
            " loading coordinator status…"
        } else {
            " unavailable on this server"
        };
        put_text(
            buffer,
            area.x,
            area.y.saturating_add(3),
            area.width,
            message,
            dim,
        );
        return;
    };
    let rows = section_rows(spaces_layout);
    for (index, row) in rows.iter().enumerate() {
        let y = area.y.saturating_add(3).saturating_add(index as u16);
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        let label = row_label(*row, info, settings.notify());
        draw_row(buffer, rect, &label, index == selected, false, palette);
        hits.push((rect, index));
    }
    let mut y = area.y.saturating_add(3).saturating_add(rows.len() as u16);
    let now = unix_now();
    let mut fact = |label: &str, value: &str| {
        draw_fact(buffer, area, y, label, value, palette);
        y = y.saturating_add(1);
    };
    fact("state", &state_text(info));
    if let Some(notice) = info.notice.as_deref() {
        fact("notice", notice);
    }
    fact("dashboard", &dashboard_text(info));
    fact("managed", &info.managed.len().to_string());
    fact("wakes", &wakes_text(info));
    let last_wake = match info.wake.last_at {
        Some(at) => format!(
            "{} ({} ago)",
            super::news::local_hhmm(at),
            super::news::ago(at, now)
        ),
        None => "none yet".to_owned(),
    };
    fact("last wake", &last_wake);
    fact(
        "session",
        info.coordinator_session.as_deref().unwrap_or("none"),
    );
    fact("dir", &info.coordinator_dir);
}
