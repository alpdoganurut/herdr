//! The settings overlay's `news` section (fork): the AI news desk's schedule
//! and state, from `news.get`.
//!
//! The rows act on Enter or a click: `scheduled runs: on|off` toggles
//! `news.enabled` on the active server (`news.set_enabled`), one row per
//! scheduled time (`08:00`, …) opens the fork's time picker (the daily
//! reminder's 30-minute grid, `settings_daily_time`) to change it and
//! Delete or Backspace removes it, `add time` opens the same picker for a
//! new one, `quiet hours …` picks the window in which a notification waits
//! (it no longer affects the schedule) and `run now` starts a run
//! (`news.run`). Every change to the times goes to the active server as
//! `news.set_times` with the whole sorted list, and the quiet-hours row as
//! `news.set_quiet_hours`, so a remote server's own config changes; the
//! reply refreshes the section (a server without `news.set_quiet_hours`
//! gets the local config write of older builds). Below the rows the model,
//! the last run and the next run are shown.

use super::render::put_text;
use super::settings_daily_time::{daily_time_choices, ClientDailyTimeChoice};
use super::*;
use crate::api::schema::NewsGetInfo;
use crossterm::event::KeyModifiers;

/// The quiet-hours picker's windows; the empty one is `off`.
pub(super) const QUIET_CHOICES: &[&str] = &[
    "",
    "00:00-08:00",
    "22:00-08:00",
    "23:00-07:00",
    "00:00-09:00",
];

const ROW_ENABLED: usize = 0;
/// The first time row; one row per scheduled time follows.
const ROW_FIRST_TIME: usize = 1;
/// Rows after the time rows: `add time`, `quiet hours`, `run now`.
const TAIL_ROWS: usize = 3;
/// Where the `add time` picker's cursor starts.
const ADD_TIME_DEFAULT: u32 = 12 * 60;

/// The section's row positions for a record with `times` scheduled times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NewsRows {
    times: usize,
}

impl NewsRows {
    fn of(info: &NewsGetInfo) -> Self {
        Self {
            times: info.times.len(),
        }
    }

    fn add(self) -> usize {
        ROW_FIRST_TIME + self.times
    }

    fn quiet(self) -> usize {
        self.add() + 1
    }

    fn run(self) -> usize {
        self.add() + 2
    }

    fn count(self) -> usize {
        self.add() + TAIL_ROWS
    }

    /// The index into the times list of a time row.
    fn time_index(self, row: usize) -> Option<usize> {
        (ROW_FIRST_TIME..self.add())
            .contains(&row)
            .then(|| row - ROW_FIRST_TIME)
    }
}

/// A picker open in place of the section's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClientNewsPicker {
    /// The time picker: for the time at `editing` in the list, or a new one.
    Time {
        editing: Option<usize>,
        choices: Vec<ClientDailyTimeChoice>,
    },
    QuietHours(Vec<String>),
}

/// The section's state inside the settings overlay: its own copy of the
/// `news.get` record (the renderer sees the overlay, not the shell state).
#[derive(Debug, Default)]
pub(super) struct ClientNewsSettings {
    pub(super) info: Option<NewsGetInfo>,
    pub(super) loading: bool,
    pub(super) picker: Option<ClientNewsPicker>,
}

/// The quiet-hours picker's rows: the configured window first when it is
/// off the list.
pub(super) fn quiet_choices(configured: &str) -> Vec<String> {
    let configured = configured.trim();
    let mut choices = Vec::with_capacity(QUIET_CHOICES.len() + 1);
    if !QUIET_CHOICES.contains(&configured) {
        choices.push(configured.to_owned());
    }
    choices.extend(QUIET_CHOICES.iter().map(|window| (*window).to_owned()));
    choices
}

/// `off` for no window, else the window itself.
pub(super) fn quiet_label(window: &str) -> &str {
    if window.trim().is_empty() {
        "off"
    } else {
        window.trim()
    }
}

/// The list to send after replacing (`editing`) or appending a time:
/// sorted, `HH:MM`, without duplicates. An entry the client cannot parse
/// is kept as it came (the server validates).
pub(super) fn times_with(times: &[String], editing: Option<usize>, minutes: u32) -> Vec<String> {
    let time = crate::config::format_hhmm(u16::try_from(minutes % (24 * 60)).unwrap_or(0));
    let mut times = times.to_vec();
    match editing {
        Some(index) if index < times.len() => times[index] = time,
        _ => times.push(time),
    }
    match crate::config::normalize_times(times.iter().map(String::as_str)) {
        Ok(minutes) => minutes
            .into_iter()
            .map(crate::config::format_hhmm)
            .collect(),
        Err(_) => times,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

impl ClientShellState {
    fn news_settings(&self) -> Option<&ClientNewsSettings> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings)) => Some(&settings.news),
            _ => None,
        }
    }

    fn news_settings_mut(&mut self) -> Option<&mut ClientNewsSettings> {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Settings(settings)) => Some(&mut settings.news),
            _ => None,
        }
    }

    /// Rows in the news section: the picker's while one is open, else the
    /// toggle, one per time, `add time`, `quiet hours` and `run now` (none
    /// without a record).
    pub(super) fn news_section_rows(&self) -> usize {
        match self.news_settings() {
            Some(ClientNewsSettings {
                picker: Some(ClientNewsPicker::Time { choices, .. }),
                ..
            }) => choices.len(),
            Some(ClientNewsSettings {
                picker: Some(ClientNewsPicker::QuietHours(choices)),
                ..
            }) => choices.len(),
            Some(ClientNewsSettings {
                info: Some(info), ..
            }) => NewsRows::of(info).count(),
            _ => 0,
        }
    }

    /// Entering the section: show what the shell already knows and pull a
    /// fresh record.
    pub(super) fn enter_news_section(&mut self, outcome: &mut ClientShellInput) {
        let info = self.news.info.clone();
        if let Some(news) = self.news_settings_mut() {
            news.info = info;
            news.picker = None;
            news.loading = true;
        }
        self.pull_news_now(outcome);
    }

    /// A `news.get` reply (or its failure) reaches the open section. The
    /// cursor stays within the rows (a removed time shortens the list).
    pub(super) fn sync_news_settings(&mut self) {
        let info = self.news.info.clone();
        let rows = info.as_ref().map(|info| NewsRows::of(info).count());
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return;
        };
        settings.news.info = info;
        settings.news.loading = false;
        if settings.section == ClientSettingsSection::News && settings.news.picker.is_none() {
            if let Some(rows) = rows {
                settings.selected = settings.selected.min(rows.saturating_sub(1));
            }
        }
    }

    /// Enter or a click on the section's current row.
    pub(super) fn apply_news_choice(&mut self, selected: usize, outcome: &mut ClientShellInput) {
        let Some(news) = self.news_settings() else {
            return;
        };
        let Some(info) = news.info.clone() else {
            return;
        };
        match news.picker.clone() {
            Some(ClientNewsPicker::Time { editing, choices }) => {
                if let Some((_, minutes)) = choices.get(selected).cloned() {
                    let times = times_with(&info.times, editing, minutes);
                    self.close_news_picker();
                    self.send_news_times(times, outcome);
                }
                return;
            }
            Some(ClientNewsPicker::QuietHours(choices)) => {
                // The active server writes its own config (`news.set_quiet_hours`,
                // so a remote server's quiet hours change too); a server
                // without the method gets the local write of older builds.
                if let Some(window) = choices.get(selected).cloned() {
                    let method = crate::api::schema::Method::NewsSetQuietHours(
                        crate::api::schema::NewsSetQuietHoursParams {
                            quiet_hours: window.clone(),
                        },
                    );
                    if self.supports_endpoint_method(&method) {
                        self.close_news_picker();
                        self.push_endpoint_method_with_kind(
                            method,
                            PendingEndpointKind::NewsSetQuietHours,
                            outcome,
                        );
                        outcome.repaint = true;
                    } else if self.save_settings_edit(
                        crate::config::ConfigEdit::NewsQuietHours(&window),
                        outcome,
                    ) {
                        self.close_news_picker();
                        self.pull_news_now(outcome);
                    }
                }
                return;
            }
            None => {}
        }
        let rows = NewsRows::of(&info);
        if selected == ROW_ENABLED {
            // The active server writes its own config and reloads it; its
            // reply refreshes the section.
            self.push_endpoint_method_with_kind(
                crate::api::schema::Method::NewsSetEnabled(
                    crate::api::schema::NewsSetEnabledParams {
                        enabled: !info.enabled,
                    },
                ),
                PendingEndpointKind::NewsSetEnabled,
                outcome,
            );
            outcome.repaint = true;
        } else if let Some(index) = rows.time_index(selected) {
            let configured = info
                .times
                .get(index)
                .and_then(|time| crate::config::parse_hhmm(time).ok())
                .map_or(ADD_TIME_DEFAULT, u32::from);
            self.open_news_time_picker(Some(index), configured, outcome);
        } else if selected == rows.add() {
            self.open_news_time_picker(None, ADD_TIME_DEFAULT, outcome);
        } else if selected == rows.quiet() {
            let choices = quiet_choices(&info.quiet_hours);
            let cursor = choices
                .iter()
                .position(|window| window == info.quiet_hours.trim())
                .unwrap_or(0);
            if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
                settings.news.picker = Some(ClientNewsPicker::QuietHours(choices));
                settings.selected = cursor;
            }
            outcome.repaint = true;
        } else if selected == rows.run() {
            self.push_endpoint_method_with_kind(
                crate::api::schema::Method::NewsRun(crate::api::schema::EmptyParams::default()),
                PendingEndpointKind::NewsRun,
                outcome,
            );
            outcome.repaint = true;
        }
    }

    /// Open the time picker for the time at `editing` (or a new one), the
    /// cursor on `configured` (minutes past midnight).
    fn open_news_time_picker(
        &mut self,
        editing: Option<usize>,
        configured: u32,
        outcome: &mut ClientShellInput,
    ) {
        let choices = daily_time_choices(configured);
        let cursor = choices
            .iter()
            .position(|(_, minutes)| *minutes == configured)
            .unwrap_or(0);
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            settings.news.picker = Some(ClientNewsPicker::Time { editing, choices });
            settings.selected = cursor;
        }
        outcome.repaint = true;
    }

    /// Send the whole list to the active server (`news.set_times`).
    fn send_news_times(&mut self, times: Vec<String>, outcome: &mut ClientShellInput) {
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::NewsSetTimes(crate::api::schema::NewsSetTimesParams {
                times,
            }),
            PendingEndpointKind::NewsSetTimes,
            outcome,
        );
        outcome.repaint = true;
    }

    /// The section's own keys, ahead of the overlay's: Delete or Backspace
    /// on a time row removes that time. Everything else falls through.
    pub(super) fn route_news_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(code, KeyCode::Delete | KeyCode::Backspace) || !modifiers.is_empty() {
            return false;
        }
        let (info, selected) = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings))
                if settings.section == ClientSettingsSection::News
                    && settings.news.picker.is_none() =>
            {
                match settings.news.info.as_ref() {
                    Some(info) => (info.clone(), settings.selected),
                    None => return false,
                }
            }
            _ => return false,
        };
        let Some(index) = NewsRows::of(&info).time_index(selected) else {
            return false;
        };
        let mut times = info.times.clone();
        times.remove(index);
        self.send_news_times(times, outcome);
        true
    }

    /// Leave an open picker for the section's rows, the cursor back on the
    /// row that opened it. Returns whether a picker was open.
    pub(super) fn close_news_picker(&mut self) -> bool {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return false;
        };
        let Some(picker) = settings.news.picker.take() else {
            return false;
        };
        let rows = NewsRows {
            times: settings
                .news
                .info
                .as_ref()
                .map_or(0, |info| info.times.len()),
        };
        settings.selected = match picker {
            ClientNewsPicker::Time {
                editing: Some(index),
                ..
            } => ROW_FIRST_TIME + index,
            ClientNewsPicker::Time { editing: None, .. } => rows.add(),
            ClientNewsPicker::QuietHours(_) => rows.quiet(),
        };
        true
    }

    /// Every row of the news section acts on a click.
    pub(super) fn news_click_applies(&self) -> bool {
        matches!(
            self.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::News,
                ..
            }))
        )
    }
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
            .fg(panel_contrast_fg(palette))
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
        &format!("   {label:<9}"),
        dim,
    );
    let x = area.x.saturating_add(12);
    put_text(buffer, x, y, area.right().saturating_sub(x), value, text);
}

/// The last run, one line: `09:14 · ok · edition 12`, or the failure.
pub(super) fn last_run_text(info: &NewsGetInfo) -> String {
    let Some(last) = info.last_run.as_ref() else {
        return "none yet".into();
    };
    let at = super::news::local_hhmm(last.ended_at.unwrap_or(last.started_at));
    let mut text = format!("{at} · {}", last.outcome);
    if let Some(edition) = last.edition {
        text.push_str(&format!(" · edition {edition}"));
    }
    if last.changed {
        text.push_str(" · changed");
    }
    if let Some(error) = last.error.as_deref().filter(|_| last.outcome != "ok") {
        text.push_str(&format!(" · {error}"));
    }
    text
}

/// The next run, one line: the time, `off` while paused, `none` without
/// times, or the run in flight.
pub(super) fn next_run_text(info: &NewsGetInfo, now: u64) -> String {
    if let Some(run) = info.run.as_ref() {
        return format!(
            "running {} ({})",
            super::news::running_for(run.started_at, now),
            run.trigger
        );
    }
    if !info.enabled {
        return "off (scheduling paused)".into();
    }
    if info.times.is_empty() {
        return "none (no times)".into();
    }
    match info.next_run_at {
        Some(at) if at <= now => "due now".into(),
        Some(at) => super::news::local_hhmm(at),
        None => "—".into(),
    }
}

/// The section: a title, the rows (or the open picker), then the facts
/// (next run, last run, model, failures).
pub(super) fn render_news_section(
    buffer: &mut Buffer,
    area: Rect,
    settings: &ClientSettingsOverlay,
    palette: &Palette,
    hits: &mut Vec<(Rect, usize)>,
) {
    let title_style = Style::default()
        .fg(palette.text)
        .bg(palette.panel_bg)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(palette.overlay1).bg(palette.panel_bg);
    let news = &settings.news;
    if let Some(picker) = news.picker.as_ref() {
        let (title, help, rows): (&str, &str, Vec<(String, bool)>) = match picker {
            ClientNewsPicker::Time { editing, choices } => {
                let current = editing
                    .and_then(|index| news.info.as_ref()?.times.get(index))
                    .and_then(|time| crate::config::parse_hhmm(time).ok())
                    .map(u32::from);
                (
                    if editing.is_some() {
                        "run at"
                    } else {
                        "add a time"
                    },
                    "local time of a scheduled run; ↵ keeps it, esc goes back",
                    choices
                        .iter()
                        .map(|(label, minutes)| (label.clone(), current == Some(*minutes)))
                        .collect(),
                )
            }
            ClientNewsPicker::QuietHours(choices) => (
                "quiet hours",
                "local window in which a news notification waits; ↵ keeps it, esc goes back",
                choices
                    .iter()
                    .map(|window| {
                        (
                            quiet_label(window).to_owned(),
                            news.info
                                .as_ref()
                                .is_some_and(|info| info.quiet_hours.trim() == window),
                        )
                    })
                    .collect(),
            ),
        };
        put_text(buffer, area.x, area.y, area.width, title, title_style);
        put_text(buffer, area.x, area.y + 1, area.width, help, dim);
        // The time picker has 48 rows: scroll so the cursor stays visible.
        let list = Rect::new(
            area.x,
            area.y + 3,
            area.width,
            area.height.saturating_sub(3),
        );
        let visible = usize::from(list.height);
        let scroll = settings.selected.saturating_sub(visible.saturating_sub(1));
        for (row, (index, (label, current))) in rows
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible)
            .enumerate()
        {
            let rect = Rect::new(list.x, list.y + row as u16, list.width, 1);
            draw_row(
                buffer,
                rect,
                label,
                index == settings.selected,
                *current,
                palette,
            );
            hits.push((rect, index));
        }
        return;
    }

    put_text(buffer, area.x, area.y, area.width, "news desk", title_style);
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        "AI news runs in the News tab; ↵ toggles, picks, runs; del removes a time",
        dim,
    );
    let Some(info) = news.info.as_ref() else {
        let message = if news.loading {
            " loading news status…"
        } else {
            " news desk unavailable on this server"
        };
        put_text(buffer, area.x, area.y + 3, area.width, message, dim);
        return;
    };
    let mut rows = Vec::with_capacity(NewsRows::of(info).count());
    rows.push(format!(
        "scheduled runs: {}",
        if info.enabled { "on" } else { "off" }
    ));
    rows.extend(info.times.iter().cloned());
    rows.push("add time".to_owned());
    rows.push(format!(
        "quiet hours {} (notifications)",
        quiet_label(&info.quiet_hours)
    ));
    rows.push("run now".to_owned());
    for (index, label) in rows.iter().enumerate() {
        let y = area.y + 3 + index as u16;
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        draw_row(
            buffer,
            rect,
            label,
            index == settings.selected,
            false,
            palette,
        );
        hits.push((rect, index));
    }
    // Straight under the rows, the next run first: a short popup with many
    // times cuts the facts from the bottom.
    let facts_y = area.y + 3 + rows.len() as u16;
    let now = unix_now();
    draw_fact(
        buffer,
        area,
        facts_y,
        "next run",
        &next_run_text(info, now),
        palette,
    );
    draw_fact(
        buffer,
        area,
        facts_y + 1,
        "last run",
        &last_run_text(info),
        palette,
    );
    draw_fact(
        buffer,
        area,
        facts_y + 2,
        "model",
        info.model.as_deref().unwrap_or("default"),
        palette,
    );
    if info.consecutive_failures > 0 {
        draw_fact(
            buffer,
            area,
            facts_y + 3,
            "failures",
            &format!("{} in a row", info.consecutive_failures),
            palette,
        );
    }
}
