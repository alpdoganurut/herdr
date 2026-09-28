//! The settings overlay's `closed` section: recently closed agent sessions
//! (`session.closed_list`), newest first, one row per session
//! `label · group · dir · 2h ago` (the age by this client's clock).
//!
//! Typing filters the rows case-insensitively on the label, group and
//! directory name, as shown (Backspace edits the filter, Esc clears it before it closes the
//! overlay). Enter reopens the selected session (`session.closed_reopen`)
//! and closes the overlay; Delete, or `d` while the filter is empty, removes
//! it (`session.closed_remove`) and refreshes the list.

use super::render::{display_width, put_text};
use super::*;
use crate::api::schema::ClosedSessionInfo;
use crossterm::event::KeyModifiers;

/// The section's state inside the settings overlay.
#[derive(Debug, Default)]
pub(super) struct ClientClosedSessions {
    /// The `session.closed_list` result, newest first; `None` until one
    /// arrives.
    pub(super) sessions: Option<Vec<ClosedSessionInfo>>,
    pub(super) loading: bool,
    /// The type-to-filter text.
    pub(super) filter: String,
}

/// The rows the section lists: the fetched sessions matching the filter,
/// newest first.
pub(super) fn filtered_closed_sessions(
    settings: &ClientSettingsOverlay,
) -> Vec<&ClosedSessionInfo> {
    let needle = settings.closed.filter.to_lowercase();
    settings
        .closed
        .sessions
        .iter()
        .flatten()
        .filter(|entry| {
            needle.is_empty()
                || [entry.title(), entry.space_name.as_str(), entry.dir_name()]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&needle))
        })
        .collect()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// One list row: `label · group · dir · 2h ago`.
pub(super) fn closed_session_row(entry: &ClosedSessionInfo, now: u64) -> String {
    format!(
        "{} · {} · {} · {}",
        entry.title(),
        entry.space_name,
        entry.dir_name(),
        crate::api::schema::compact_closed_age(now.saturating_sub(entry.closed_at))
    )
}

impl ClientShellState {
    fn closed_settings_mut(&mut self) -> Option<&mut ClientSettingsOverlay> {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Settings(settings))
                if settings.section == ClientSettingsSection::ClosedSessions =>
            {
                Some(settings)
            }
            _ => None,
        }
    }

    /// Ask the endpoint for the list; the section shows `loading` meanwhile
    /// and `unavailable` when the request cannot be sent or fails.
    pub(super) fn queue_closed_sessions(&mut self, outcome: &mut ClientShellInput) {
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            settings.closed.loading = true;
        }
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::SessionClosedList(
                crate::api::schema::EmptyParams::default(),
            ),
            PendingEndpointKind::SessionClosedList,
            outcome,
        ) {
            if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
                settings.closed.loading = false;
            }
        }
    }

    fn selected_closed_session_id(&self) -> Option<String> {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_ref() else {
            return None;
        };
        filtered_closed_sessions(settings)
            .get(settings.selected)
            .map(|entry| entry.id.clone())
    }

    /// Enter: reopen the selected session and close the overlay.
    pub(super) fn reopen_selected_closed_session(&mut self, outcome: &mut ClientShellInput) {
        let Some(id) = self.selected_closed_session_id() else {
            return;
        };
        if self.push_endpoint_method_with_kind(
            crate::api::schema::Method::SessionClosedReopen(
                crate::api::schema::ClosedSessionTarget { id },
            ),
            PendingEndpointKind::SessionClosedReopen,
            outcome,
        ) {
            self.cancel_settings_overlay();
        }
        outcome.repaint = true;
    }

    /// Delete / `d`: drop the selected entry (shown gone at once, then the
    /// list is fetched again once the endpoint answers).
    fn remove_selected_closed_session(&mut self, outcome: &mut ClientShellInput) {
        let Some(id) = self.selected_closed_session_id() else {
            return;
        };
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::SessionClosedRemove(
                crate::api::schema::ClosedSessionTarget { id: id.clone() },
            ),
            PendingEndpointKind::SessionClosedRemove,
            outcome,
        ) {
            outcome.repaint = true;
            return;
        }
        if let Some(settings) = self.closed_settings_mut() {
            if let Some(sessions) = settings.closed.sessions.as_mut() {
                sessions.retain(|entry| entry.id != id);
            }
            let count = filtered_closed_sessions(settings).len();
            settings.selected = settings.selected.min(count.saturating_sub(1));
        }
        outcome.repaint = true;
    }

    fn edit_closed_filter(&mut self, edit: impl FnOnce(&mut String)) {
        if let Some(settings) = self.closed_settings_mut() {
            edit(&mut settings.closed.filter);
            settings.selected = 0;
        }
    }

    /// The section's own keys, ahead of the overlay's: filter text, Esc on
    /// a non-empty filter, Backspace and removal. Everything else (arrows,
    /// Tab, Enter, Esc on an empty filter) falls through.
    pub(super) fn route_closed_sessions_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(settings) = self.closed_settings_mut() else {
            return false;
        };
        let filter_empty = settings.closed.filter.is_empty();
        match code {
            KeyCode::Esc if !filter_empty => {
                self.edit_closed_filter(String::clear);
            }
            KeyCode::Backspace => {
                self.edit_closed_filter(|filter| {
                    filter.pop();
                });
            }
            KeyCode::Delete => self.remove_selected_closed_session(outcome),
            KeyCode::Char('d') if filter_empty && modifiers.is_empty() => {
                self.remove_selected_closed_session(outcome);
            }
            KeyCode::Char(ch) if modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.edit_closed_filter(|filter| filter.push(ch));
            }
            _ => return false,
        }
        outcome.repaint = true;
        true
    }

    pub(super) fn handle_closed_sessions_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        match kind {
            PendingEndpointKind::SessionClosedList => {
                let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
                    return (false, Vec::new());
                };
                settings.closed.loading = false;
                match result {
                    Ok(crate::api::schema::ResponseResult::SessionClosedList { sessions }) => {
                        settings.closed.sessions = Some(sessions);
                        let count = filtered_closed_sessions(settings).len();
                        settings.selected = settings.selected.min(count.saturating_sub(1));
                    }
                    Ok(_) => {
                        self.set_endpoint_error(
                            "endpoint returned an unexpected closed sessions result",
                        );
                    }
                    Err(_) => {}
                }
                (true, Vec::new())
            }
            PendingEndpointKind::SessionClosedRemove => {
                let actions = if self.closed_settings_mut().is_some() {
                    let mut deferred = ClientShellInput::default();
                    self.queue_closed_sessions(&mut deferred);
                    deferred.actions
                } else {
                    Vec::new()
                };
                (true, actions)
            }
            _ => (result.is_err(), Vec::new()),
        }
    }
}

/// The section: a header naming the filter, then the rows, scrolled like the
/// theme list so the selection stays visible.
pub(super) fn render_closed_sessions(
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
    let title = "recently closed";
    put_text(buffer, area.x, area.y, area.width, title, title_style);
    if !settings.closed.filter.is_empty() {
        let x = area.x.saturating_add(display_width(title));
        put_text(
            buffer,
            x,
            area.y,
            area.right().saturating_sub(x),
            &format!(" · filter: {}", settings.closed.filter),
            Style::default().fg(palette.accent).bg(palette.panel_bg),
        );
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        "type to filter; ↵ reopens the agent session, del removes it",
        dim,
    );

    let message = match settings.closed.sessions.as_ref() {
        None if settings.closed.loading => Some(" loading closed sessions…"),
        None => Some(" closed sessions unavailable"),
        Some(sessions) if sessions.is_empty() => Some(" no closed agent sessions"),
        Some(_) => None,
    };
    if let Some(message) = message {
        put_text(buffer, area.x, area.y + 3, area.width, message, dim);
        return;
    }
    let rows = filtered_closed_sessions(settings);
    if rows.is_empty() {
        put_text(
            buffer,
            area.x,
            area.y + 3,
            area.width,
            " nothing matches the filter",
            dim,
        );
        return;
    }
    let list = Rect::new(
        area.x,
        area.y + 3,
        area.width,
        area.height.saturating_sub(3),
    );
    let visible = usize::from(list.height);
    let scroll = settings.selected.saturating_sub(visible.saturating_sub(1));
    let now = unix_now();
    for (row, (index, entry)) in rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .enumerate()
    {
        let rect = Rect::new(list.x, list.y + row as u16, list.width, 1);
        let selected = index == settings.selected;
        let style = if selected {
            let fg = match palette.panel_bg {
                ratatui::style::Color::Reset => palette.surface_dim,
                color => color,
            };
            Style::default()
                .fg(fg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.panel_bg)
        };
        buffer.set_style(rect, style);
        let marker = if selected { "▸" } else { " " };
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(" {marker} {}", closed_session_row(entry, now)),
            style,
        );
        hits.push((rect, index));
    }
}
