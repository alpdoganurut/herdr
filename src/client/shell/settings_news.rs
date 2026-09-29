//! The settings overlay's `news` section (fork): the AI news desk's schedule
//! and state, from `news.get`.
//!
//! Four rows act on Enter or a click: `scheduled runs: on|off` toggles
//! `news.enabled` on the active server (`news.set_enabled`, so a remote
//! server's own config changes), `every N h` and `quiet hours …` open a
//! picker in place of the rows (`3 / 6 / 12 / 24 h`; `off` and four windows,
//! a configured value off the list first), and `run now` starts a run
//! (`news.run`). Below them the model, the last run and the next run are
//! shown. The interval and quiet-hours rows persist like the other settings
//! (`ConfigEdit` on the local config + `server.reload_config`), then the
//! section pulls `news.get` again.

use super::render::put_text;
use super::*;
use crate::api::schema::NewsGetInfo;

/// The interval picker's hours.
pub(super) const INTERVAL_CHOICES: &[u32] = &[3, 6, 12, 24];
/// The quiet-hours picker's windows; the empty one is `off`.
pub(super) const QUIET_CHOICES: &[&str] = &[
    "",
    "00:00-08:00",
    "22:00-08:00",
    "23:00-07:00",
    "00:00-09:00",
];

const ROW_ENABLED: usize = 0;
const ROW_INTERVAL: usize = 1;
const ROW_QUIET: usize = 2;
const ROW_RUN: usize = 3;
const ROWS: usize = 4;

/// A picker open in place of the section's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClientNewsPicker {
    Interval(Vec<u32>),
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

/// The interval picker's rows: the configured value first when it is off
/// the list.
pub(super) fn interval_choices(configured: u32) -> Vec<u32> {
    let mut choices = Vec::with_capacity(INTERVAL_CHOICES.len() + 1);
    if !INTERVAL_CHOICES.contains(&configured) {
        choices.push(configured);
    }
    choices.extend_from_slice(INTERVAL_CHOICES);
    choices
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
    /// four action rows (none without a record).
    pub(super) fn news_section_rows(&self) -> usize {
        match self.news_settings() {
            Some(ClientNewsSettings {
                picker: Some(ClientNewsPicker::Interval(choices)),
                ..
            }) => choices.len(),
            Some(ClientNewsSettings {
                picker: Some(ClientNewsPicker::QuietHours(choices)),
                ..
            }) => choices.len(),
            Some(ClientNewsSettings { info: Some(_), .. }) => ROWS,
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

    /// A `news.get` reply (or its failure) reaches the open section.
    pub(super) fn sync_news_settings(&mut self) {
        let info = self.news.info.clone();
        if let Some(news) = self.news_settings_mut() {
            news.info = info;
            news.loading = false;
        }
    }

    /// Enter or a click on the section's current row.
    pub(super) fn apply_news_choice(&mut self, selected: usize, outcome: &mut ClientShellInput) {
        let Some(news) = self.news_settings() else {
            return;
        };
        // The pickers write the local config file (no server method for the
        // interval or quiet hours yet); a remote server keeps its own.
        match news.picker.clone() {
            Some(ClientNewsPicker::Interval(choices)) => {
                if let Some(hours) = choices.get(selected).copied() {
                    if self.save_settings_edit(
                        crate::config::ConfigEdit::NewsIntervalHours(hours),
                        outcome,
                    ) {
                        self.close_news_picker();
                        self.pull_news_now(outcome);
                    }
                }
                return;
            }
            Some(ClientNewsPicker::QuietHours(choices)) => {
                if let Some(window) = choices.get(selected) {
                    if self.save_settings_edit(
                        crate::config::ConfigEdit::NewsQuietHours(window),
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
        let Some(info) = news.info.clone() else {
            return;
        };
        match selected {
            ROW_ENABLED => {
                // The active server writes its own config and reloads it;
                // its reply refreshes the section.
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
            }
            ROW_INTERVAL => {
                let choices = interval_choices(info.interval_hours);
                let cursor = choices
                    .iter()
                    .position(|hours| *hours == info.interval_hours)
                    .unwrap_or(0);
                if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
                    settings.news.picker = Some(ClientNewsPicker::Interval(choices));
                    settings.selected = cursor;
                }
                outcome.repaint = true;
            }
            ROW_QUIET => {
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
            }
            ROW_RUN => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::NewsRun(crate::api::schema::EmptyParams::default()),
                    PendingEndpointKind::NewsRun,
                    outcome,
                );
                outcome.repaint = true;
            }
            _ => {}
        }
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
        settings.selected = match picker {
            ClientNewsPicker::Interval(_) => ROW_INTERVAL,
            ClientNewsPicker::QuietHours(_) => ROW_QUIET,
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

/// The next run, one line: the time, `off` while paused, or the run in
/// flight.
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
    match info.next_run_at {
        Some(at) if at <= now => "due now".into(),
        Some(at) => super::news::local_hhmm(at),
        None => "—".into(),
    }
}

/// The section: a title, the four rows (or the open picker), then the facts.
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
            ClientNewsPicker::Interval(choices) => (
                "run every",
                "hours between scheduled news runs; ↵ keeps it, esc goes back",
                choices
                    .iter()
                    .map(|hours| {
                        (
                            format!("{hours} h"),
                            news.info
                                .as_ref()
                                .is_some_and(|info| info.interval_hours == *hours),
                        )
                    })
                    .collect(),
            ),
            ClientNewsPicker::QuietHours(choices) => (
                "quiet hours",
                "local window in which a due run waits; ↵ keeps it, esc goes back",
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
        for (index, (label, current)) in rows.iter().enumerate() {
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
        "scheduled AI news runs in the News tab; ↵ toggles, picks or runs",
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
    let rows = [
        format!(
            "scheduled runs: {}",
            if info.enabled { "on" } else { "off" }
        ),
        format!("every {} h", info.interval_hours),
        format!("quiet hours {}", quiet_label(&info.quiet_hours)),
        "run now".to_owned(),
    ];
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
    let facts_y = area.y + 3 + ROWS as u16 + 1;
    let now = unix_now();
    draw_fact(
        buffer,
        area,
        facts_y,
        "model",
        info.model.as_deref().unwrap_or("default"),
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
        "next run",
        &next_run_text(info, now),
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
