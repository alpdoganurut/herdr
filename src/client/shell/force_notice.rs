//! Force suspend (fork): when the user's own suspend or restart (the tab
//! menu, `keys.toggle_agent_suspend`, `keys.restart_agent`) is refused for a
//! reason a forced exit overrides — the agent is blocked or working, or its
//! subagents or background tasks run — the refusal notice carries one
//! button, `Force suspend` or `Force restart`. One click sends
//! `agent.suspend_force` / `agent.restart_force` (no second confirm) and
//! closes the notice. Other refusals (no session yet, no resume plan, the
//! pane's own process, not running) keep the plain notice.
//!
//! Mouse only: endpoint notices have no key path. The button shows on the
//! desktop card, not on the mobile banner, and only while the server
//! advertises the force method; an older server keeps today's notice.

use super::render::put_text;
use super::*;
use crate::api::schema::{AgentRestartParams, AgentSuspendParams, Method};
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::widgets::{Block, Borders, Clear, Widget};

/// The refusal codes a forced exit overrides.
pub(super) const FORCEABLE_CODES: [&str; 4] = [
    "agent_blocked",
    "agent_working",
    "agent_subagents_running",
    "background_tasks_running",
];

/// How long a notice with a force button stays: a plain rejection's three
/// seconds are too short to aim at.
const FORCE_NOTICE_DURATION: std::time::Duration = std::time::Duration::from_secs(8);

/// A refusal notice's force button: its label and the method a click sends.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ClientNoticeForce {
    pub(super) label: &'static str,
    pub(super) method: Method,
}

/// The pending kind of a request: a plain suspend or restart remembers its
/// target, so its refusal can offer to force it.
pub(super) fn pending_kind(method: &Method, kind: PendingEndpointKind) -> PendingEndpointKind {
    match (kind, method) {
        (PendingEndpointKind::Generic, Method::AgentSuspend(params)) => {
            PendingEndpointKind::ForceableAgentStop {
                target: params.target.clone(),
                restart: false,
            }
        }
        (PendingEndpointKind::Generic, Method::AgentRestart(params)) => {
            PendingEndpointKind::ForceableAgentStop {
                target: params.target.clone(),
                restart: true,
            }
        }
        (kind, _) => kind,
    }
}

/// The forced method for a refused suspend or restart of `target`.
pub(super) fn force_method(target: String, restart: bool) -> Method {
    if restart {
        Method::AgentRestartForce(AgentRestartParams { target })
    } else {
        Method::AgentSuspendForce(AgentSuspendParams { target })
    }
}

impl ClientShellState {
    /// After a refusal notice was pushed for `kind`: give it the force
    /// button when the refusal is one a forced exit overrides and the server
    /// offers the force method.
    pub(super) fn attach_force_action(
        &mut self,
        kind: &PendingEndpointKind,
        code: &str,
        notice_code: &str,
    ) {
        let PendingEndpointKind::ForceableAgentStop { target, restart } = kind else {
            return;
        };
        if !FORCEABLE_CODES.contains(&code) {
            return;
        }
        let method = force_method(target.clone(), *restart);
        if !self.supports_endpoint_method(&method) {
            return;
        }
        let Some(notice) = self
            .visible_endpoint_notice
            .as_mut()
            .filter(|notice| notice.key.code == notice_code)
        else {
            return;
        };
        notice.force = Some(ClientNoticeForce {
            label: if *restart {
                "Force restart"
            } else {
                "Force suspend"
            },
            method,
        });
        notice.deadline = notice
            .deadline
            .max(std::time::Instant::now() + FORCE_NOTICE_DURATION);
    }

    /// A left press on the notice's force button: send the forced method and
    /// close the notice. `false` leaves the press to the notice itself.
    pub(super) fn handle_force_notice_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left)
            || !super::contains(
                self.hits.notification_toast_action,
                (mouse.column, mouse.row),
            )
        {
            return false;
        }
        let Some(force) = self
            .visible_endpoint_notice
            .as_mut()
            .and_then(|notice| notice.force.take())
        else {
            return false;
        };
        self.visible_endpoint_notice = None;
        self.hits.notification_toast_action = Rect::default();
        outcome.repaint = true;
        self.push_endpoint_method(force.method, outcome);
        true
    }
}

/// The notice card with its force button on its own row under the body,
/// laid out like `notifications::render_notification_card` (top right,
/// beside the info dock). Returns the card and the button.
pub(super) fn render_card(
    buffer: &mut Buffer,
    area: Rect,
    dock: Rect,
    notice: &ClientVisibleEndpointNotice,
    force: &ClientNoticeForce,
    top_offset: u16,
    dot_color: ratatui::style::Color,
    palette: &Palette,
) -> (Rect, Rect) {
    if area.is_empty() {
        return (Rect::default(), Rect::default());
    }
    let label = format!(" {} ", force.label);
    let width_of = unicode_width::UnicodeWidthStr::width;
    let content_width = width_of(notice.title.as_str())
        .max(width_of(notice.body.as_str()))
        .max(width_of(label.as_str()))
        .saturating_add(6);
    let area = super::notifications::toast_area_beside_dock(
        area,
        dock,
        crate::config::ToastHerdrPosition::TopRight,
        content_width,
    );
    let width = u16::try_from(content_width)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let height: u16 = if notice.body.is_empty() { 4 } else { 5 }.min(area.height);
    let x = area.right().saturating_sub(width);
    let max_y = area.bottom().saturating_sub(height).max(area.y);
    let y = area.y.saturating_add(top_offset).clamp(area.y, max_y);
    let rect = Rect::new(x, y, width, height);
    Clear.render(rect, buffer);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.overlay0))
        .style(Style::default().bg(palette.panel_bg));
    let inner = block.inner(rect);
    block.render(rect, buffer);
    if inner.is_empty() {
        return (rect, Rect::default());
    }
    put_text(
        buffer,
        inner.x,
        inner.y,
        1,
        "●",
        Style::default().fg(dot_color).bg(palette.panel_bg),
    );
    put_text(
        buffer,
        inner.x.saturating_add(2),
        inner.y,
        inner.width.saturating_sub(2),
        &notice.title,
        Style::default()
            .fg(palette.text)
            .bg(palette.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let mut row = inner.y.saturating_add(1);
    if !notice.body.is_empty() && row < inner.bottom() {
        put_text(
            buffer,
            inner.x.saturating_add(2),
            row,
            inner.width.saturating_sub(2),
            &notice.body,
            Style::default().fg(palette.overlay0).bg(palette.panel_bg),
        );
        row = row.saturating_add(1);
    }
    if row >= inner.bottom() {
        return (rect, Rect::default());
    }
    let button_x = inner.x.saturating_add(2);
    let button_width = u16::try_from(width_of(label.as_str()))
        .unwrap_or(u16::MAX)
        .min(inner.right().saturating_sub(button_x));
    let button = Rect::new(button_x, row, button_width, 1);
    let accent = match palette.panel_bg {
        ratatui::style::Color::Reset => palette.surface_dim,
        color => color,
    };
    let style = Style::default()
        .fg(accent)
        .bg(palette.red)
        .add_modifier(Modifier::BOLD);
    buffer.set_style(button, style);
    put_text(buffer, button.x, button.y, button.width, &label, style);
    (rect, button)
}
