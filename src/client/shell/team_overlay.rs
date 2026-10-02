//! The Team info overlay (fork): one centred panel for a team group's
//! purpose, members (role, agent, pane, status, time in state) and removed
//! panes. Mouse-first, like the Browser overlay: a click on a member row
//! focuses its pane, `✎` opens the Rename modal for the purpose or a role
//! (the overlay reopens when that modal closes), `×` removes a member from
//! the team (`team.leave`), `+ join` brings a removed pane back
//! (`team.join`). Keys: Up/Down move, Enter acts on the row (focus, edit
//! purpose, join), `e` edits, `x` removes, Esc closes.
//!
//! The overlay keeps its own copy of the team: the push's membership until
//! its `team.get` reply arrives (status and time in state come only from
//! that reply), then the reply. It pulls on open, after every teams push
//! and on the minute tick (`teams.rs::tick_teams`).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::teams::{member_name, ClientTabTeamMenu, TeamRequest, NO_ROLE_MARK, TEAM_MARK};
use super::*;
use crate::api::schema::team::{TeamActor, TeamInfo, TeamMemberInfo};

/// How often the open overlay pulls `team.get` without a push.
pub(crate) const TEAM_OVERLAY_REFRESH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(60);

const WIDTH: u16 = 72;
const HINT: &str = "click a member to focus it · ✎ edit · × remove from team";

/// The open Team info overlay.
#[derive(Debug)]
pub(crate) struct ClientTeamOverlay {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) workspace_id: String,
    /// The group's label when the overlay opened (the title while the team
    /// is unknown).
    pub(crate) group_label: String,
    /// The last known team: the push's until a `team.get` reply arrives.
    pub(crate) team: Option<TeamInfo>,
    /// `team` came from a `team.get` reply (it has status).
    pub(crate) detailed: bool,
    /// The group is no longer a team.
    pub(crate) gone: bool,
    /// A `team.get` is in flight.
    pub(crate) loading: bool,
    /// Pull on the next tick.
    pub(crate) refresh_due: bool,
    pub(crate) last_pull: Option<std::time::Instant>,
    /// The wall clock of the last reply, for "time in state" (render stays
    /// free of clock reads).
    pub(crate) now_unix: u64,
    pub(crate) cursor: usize,
}

/// A selectable row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TeamOverlayRow {
    Purpose,
    Member(usize),
    Excluded(usize),
}

/// A mouse target in the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TeamOverlayHit {
    Focus { pane_id: String },
    EditPurpose,
    EditRole { pane_id: String },
    Remove { pane_id: String },
    Join { pane_id: String },
}

impl ClientTeamOverlay {
    pub(crate) fn new(
        endpoint_id: ClientEndpointId,
        workspace_id: String,
        group_label: String,
        team: Option<TeamInfo>,
    ) -> Self {
        Self {
            endpoint_id,
            workspace_id,
            group_label,
            gone: team.is_none(),
            team,
            detailed: false,
            loading: false,
            refresh_due: true,
            last_pull: None,
            now_unix: super::teams::unix_now(),
            cursor: 0,
        }
    }

    /// The selectable rows, top to bottom.
    pub(crate) fn rows(&self) -> Vec<TeamOverlayRow> {
        let Some(team) = self.team.as_ref() else {
            return Vec::new();
        };
        std::iter::once(TeamOverlayRow::Purpose)
            .chain((0..team.members.len()).map(TeamOverlayRow::Member))
            .chain((0..team.excluded.len()).map(TeamOverlayRow::Excluded))
            .collect()
    }

    fn selected(&self) -> Option<TeamOverlayRow> {
        self.rows().get(self.cursor).cloned()
    }

    pub(crate) fn move_cursor(&mut self, delta: isize) -> bool {
        let count = self.rows().len();
        if count == 0 {
            return false;
        }
        let next = self
            .cursor
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        let moved = next != self.cursor;
        self.cursor = next;
        moved
    }

    /// A teams push: the group's team in it (`None`: no longer a team).
    pub(crate) fn on_push(&mut self, team: Option<&TeamInfo>) {
        match team {
            None => {
                self.team = None;
                self.detailed = false;
                self.gone = true;
            }
            Some(team) => {
                if !self.detailed || self.gone {
                    self.team = Some(team.clone());
                    self.detailed = false;
                }
                self.gone = false;
                self.refresh_due = true;
            }
        }
        self.clamp_cursor();
    }

    /// A `team.get` reply: `Some(team)` (or `Some(None)`: not a team);
    /// `None` for a failed request (the state stays).
    pub(crate) fn on_get_reply(&mut self, reply: Option<Option<TeamInfo>>, now_unix: u64) {
        self.loading = false;
        self.now_unix = now_unix;
        match reply {
            Some(Some(team)) => {
                self.team = Some(team);
                self.detailed = true;
                self.gone = false;
            }
            Some(None) => {
                self.team = None;
                self.detailed = false;
                self.gone = true;
            }
            None => {}
        }
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.rows().len().saturating_sub(1));
    }

    /// The tick: the workspace to pull `team.get` for when a pull is due and
    /// may be sent. A due pull that cannot be sent is still remembered.
    pub(crate) fn pull_due(&mut self, now: std::time::Instant, advertised: bool) -> Option<String> {
        if self.loading {
            return None;
        }
        let periodic = self
            .last_pull
            .is_some_and(|last| now.duration_since(last) >= TEAM_OVERLAY_REFRESH_INTERVAL);
        if !(self.refresh_due || periodic) {
            return None;
        }
        self.refresh_due = false;
        self.last_pull = Some(now);
        if !advertised {
            return None;
        }
        self.loading = true;
        Some(self.workspace_id.clone())
    }

    fn member(&self, index: usize) -> Option<&TeamMemberInfo> {
        self.team.as_ref()?.members.get(index)
    }
}

/// `set by user` / `set by coordinator` / `set by fixer`.
fn actor_label(actor: &TeamActor) -> String {
    match actor {
        TeamActor::User => "set by user".into(),
        TeamActor::Coordinator => "set by coordinator".into(),
        TeamActor::Agent { name } => format!("set by {name}"),
        TeamActor::Unknown => String::new(),
    }
}

/// `4m`, `12m`, `3h`, `2d`.
fn age(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

impl ClientShellState {
    /// Group menu "Team info": open the overlay for a team group.
    pub(super) fn open_team_overlay(&mut self, workspace_id: String) {
        let group_label = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == workspace_id)
            })
            .map(|workspace| workspace.label.clone())
            .unwrap_or_default();
        let team = self.active_team(&workspace_id).cloned();
        self.overlay = Some(ClientShellOverlay::TeamInfo(ClientTeamOverlay::new(
            self.active_endpoint_id.clone(),
            workspace_id,
            group_label,
            team,
        )));
    }

    fn team_overlay(&self) -> Option<&ClientTeamOverlay> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::TeamInfo(overlay)) => Some(overlay),
            _ => None,
        }
    }

    /// The overlay acts on the active endpoint only (its requests go there).
    fn team_overlay_actionable(&self) -> bool {
        self.team_overlay()
            .is_some_and(|overlay| overlay.endpoint_id == self.active_endpoint_id)
    }

    /// Open the Rename modal for the purpose; it reopens the overlay.
    fn edit_team_purpose_from_overlay(&mut self) {
        let Some(overlay) = self.team_overlay() else {
            return;
        };
        let workspace_id = overlay.workspace_id.clone();
        let purpose = overlay
            .team
            .as_ref()
            .and_then(|team| team.purpose.clone())
            .unwrap_or_default();
        self.open_team_purpose_rename(workspace_id, &purpose, false, true);
    }

    /// Open the Rename modal for a member's role; it reopens the overlay.
    fn edit_team_role_from_overlay(&mut self, pane_id: String) {
        let Some(overlay) = self.team_overlay() else {
            return;
        };
        let workspace_id = overlay.workspace_id.clone();
        let role = overlay
            .team
            .as_ref()
            .and_then(|team| team.members.iter().find(|member| member.pane_id == pane_id))
            .and_then(|member| member.role.clone());
        self.open_team_role_rename(
            ClientTabTeamMenu {
                pane_id,
                workspace_id,
                member: true,
                role,
            },
            true,
        );
    }

    /// A target in the overlay, from a click or a key.
    pub(super) fn activate_team_overlay_hit(
        &mut self,
        hit: TeamOverlayHit,
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        if let TeamOverlayHit::Focus { pane_id } = hit {
            let Some(endpoint_id) = self.team_overlay().map(|o| o.endpoint_id.clone()) else {
                return;
            };
            if self.focus_or_activate(
                endpoint_id,
                ClientEndpointFocusTarget::Pane(pane_id),
                outcome,
            ) {
                self.overlay = None;
            }
            return;
        }
        if !self.team_overlay_actionable() {
            self.set_endpoint_error("team actions apply to the active server only");
            return;
        }
        match hit {
            TeamOverlayHit::Focus { .. } => {}
            TeamOverlayHit::EditPurpose => self.edit_team_purpose_from_overlay(),
            TeamOverlayHit::EditRole { pane_id } => self.edit_team_role_from_overlay(pane_id),
            TeamOverlayHit::Remove { pane_id } => {
                self.push_team_request(TeamRequest::Leave { pane_id }, outcome);
            }
            TeamOverlayHit::Join { pane_id } => {
                self.push_team_request(TeamRequest::Join { pane_id }, outcome);
            }
        }
    }

    /// What Enter / `e` / `x` act on for the selected row.
    fn team_overlay_key_hit(&self, key: char) -> Option<TeamOverlayHit> {
        let overlay = self.team_overlay()?;
        let team = overlay.team.as_ref()?;
        match (overlay.selected()?, key) {
            (TeamOverlayRow::Purpose, '\r' | 'e') => Some(TeamOverlayHit::EditPurpose),
            (TeamOverlayRow::Member(index), '\r') => Some(TeamOverlayHit::Focus {
                pane_id: overlay.member(index)?.pane_id.clone(),
            }),
            (TeamOverlayRow::Member(index), 'e') => Some(TeamOverlayHit::EditRole {
                pane_id: overlay.member(index)?.pane_id.clone(),
            }),
            (TeamOverlayRow::Member(index), 'x') => Some(TeamOverlayHit::Remove {
                pane_id: overlay.member(index)?.pane_id.clone(),
            }),
            (TeamOverlayRow::Excluded(index), '\r') => Some(TeamOverlayHit::Join {
                pane_id: team.excluded.get(index)?.clone(),
            }),
            _ => None,
        }
    }

    /// Keys while the overlay is open.
    pub(super) fn route_team_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        let hit = match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                outcome.repaint = true;
                return;
            }
            KeyCode::Up | KeyCode::Down => {
                let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                if let Some(ClientShellOverlay::TeamInfo(overlay)) = self.overlay.as_mut() {
                    outcome.repaint |= overlay.move_cursor(delta);
                }
                return;
            }
            KeyCode::Enter => self.team_overlay_key_hit('\r'),
            KeyCode::Char(character @ ('e' | 'x')) if key.modifiers.is_empty() => {
                self.team_overlay_key_hit(character)
            }
            _ => None,
        };
        if let Some(hit) = hit {
            self.activate_team_overlay_hit(hit, outcome);
        }
    }

    /// A left press while the overlay is open: a target acts, a press
    /// outside the panel closes it.
    pub(super) fn route_team_overlay_click(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) {
        if let Some((_, hit)) = self
            .hits
            .team_overlay
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .cloned()
        {
            self.activate_team_overlay_hit(hit, outcome);
            return;
        }
        if super::contains(self.hits.overlay_cancel, point)
            || !super::contains(self.hits.team_overlay_popup, point)
        {
            self.overlay = None;
            outcome.repaint = true;
        }
    }
}

/// What a render of the overlay registers for the mouse.
#[derive(Debug, Default)]
pub(crate) struct TeamOverlayRender {
    pub(crate) area: Rect,
    pub(crate) cancel: Rect,
    /// Buttons first, then rows, so the first match is the most specific.
    pub(crate) hits: Vec<(Rect, TeamOverlayHit)>,
}

fn put(b: &mut Buffer, x: u16, y: u16, width: u16, spans: Vec<Span<'_>>, style: Style) {
    if width == 0 {
        return;
    }
    Paragraph::new(Line::from(spans))
        .style(style)
        .render(Rect::new(x, y, width, 1), b);
}

fn cells(text: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

/// Draw a right-aligned button ending at `right`; returns its rect.
fn button(b: &mut Buffer, right: u16, y: u16, text: &str, style: Style) -> Rect {
    let width = cells(text);
    let x = right.saturating_sub(width);
    put(
        b,
        x,
        y,
        width,
        vec![Span::styled(text.to_owned(), style)],
        style,
    );
    Rect::new(x, y, width, 1)
}

/// The overlay. Pure: draws `overlay` and returns the mouse targets.
pub(crate) fn render_team_overlay(
    b: &mut Buffer,
    overlay: &ClientTeamOverlay,
    p: &Palette,
    status_indicators: crate::config::StatusIndicatorStyle,
) -> Option<TeamOverlayRender> {
    // Dim what is under the panel, like the other modal overlays.
    for y in b.area.y..b.area.bottom() {
        for x in b.area.x..b.area.right() {
            let cell = &mut b[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
    let (members, excluded) = overlay
        .team
        .as_ref()
        .map_or((0, 0), |team| (team.members.len(), team.excluded.len()));
    let body_rows = 2 + members.max(1) + excluded + 2;
    let height = u16::try_from(body_rows + 2).unwrap_or(u16::MAX);
    let screen = b.area;
    let width = WIDTH.min(screen.width.saturating_sub(2));
    let height = height.min(screen.height.saturating_sub(2));
    if width < 30 || height < 5 {
        return None;
    }
    let outer = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let bg = Style::default().bg(p.panel_bg);
    let border = Style::default().fg(p.accent).bg(p.panel_bg);
    for y in outer.y..outer.bottom() {
        for x in outer.x..outer.right() {
            b[(x, y)].set_symbol(" ").set_style(bg);
        }
    }
    for x in outer.x..outer.right() {
        b[(x, outer.y)].set_symbol("─").set_style(border);
        b[(x, outer.bottom() - 1)].set_symbol("─").set_style(border);
    }
    for y in outer.y..outer.bottom() {
        b[(outer.x, y)].set_symbol("│").set_style(border);
        b[(outer.right() - 1, y)].set_symbol("│").set_style(border);
    }
    b[(outer.x, outer.y)].set_symbol("╭");
    b[(outer.right() - 1, outer.y)].set_symbol("╮");
    b[(outer.x, outer.bottom() - 1)].set_symbol("╰");
    b[(outer.right() - 1, outer.bottom() - 1)].set_symbol("╯");
    let label = overlay
        .team
        .as_ref()
        .map_or(overlay.group_label.as_str(), |team| {
            team.workspace_label.as_str()
        });
    let title = crate::ui::truncate_end(
        &format!(" team · {label} "),
        usize::from(width.saturating_sub(4)),
    );
    put(
        b,
        outer.x + 1,
        outer.y,
        cells(&title),
        vec![Span::styled(
            title.clone(),
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        )],
        bg,
    );
    let esc = " esc ";
    let cancel = Rect::new(
        outer.right().saturating_sub(cells(esc) + 2),
        outer.bottom() - 1,
        cells(esc),
        1,
    );
    put(
        b,
        cancel.x,
        cancel.y,
        cancel.width,
        vec![Span::styled(
            esc,
            Style::default().fg(p.overlay1).bg(p.panel_bg),
        )],
        bg,
    );
    let inner = Rect::new(
        outer.x + 2,
        outer.y + 1,
        outer.width.saturating_sub(4),
        outer.height.saturating_sub(2),
    );
    let mut render = TeamOverlayRender {
        area: outer,
        cancel,
        hits: Vec::new(),
    };
    let dim = Style::default().fg(p.overlay1).bg(p.panel_bg);
    let text = Style::default().fg(p.text).bg(p.panel_bg);
    let accent = Style::default().fg(p.accent).bg(p.panel_bg);
    let Some(team) = overlay.team.as_ref() else {
        let message = if overlay.gone && !overlay.loading {
            "this group is not a team any more"
        } else {
            "loading…"
        };
        put(
            b,
            inner.x,
            inner.y,
            inner.width,
            vec![Span::styled(message, dim)],
            bg,
        );
        return Some(render);
    };
    let rows = overlay.rows();
    let selected = rows.get(overlay.cursor);
    let row_style = |row: &TeamOverlayRow| {
        if selected == Some(row) {
            Style::default().bg(p.active_row_bg)
        } else {
            bg
        }
    };
    let right = inner.right();
    let mut y = inner.y;

    // purpose  fix calendar sync            set by user      ✎
    let style = row_style(&TeamOverlayRow::Purpose);
    b.set_style(Rect::new(inner.x, y, inner.width, 1), style);
    let edit = button(b, right, y, "✎", accent.patch(style));
    render.hits.push((edit, TeamOverlayHit::EditPurpose));
    let by = team
        .purpose_by
        .as_ref()
        .map(actor_label)
        .unwrap_or_default();
    let by_x = right.saturating_sub(cells(&by) + 4);
    let purpose_room = usize::from(by_x.saturating_sub(inner.x + 10));
    let purpose = match team.purpose.as_deref() {
        Some(purpose) if !purpose.trim().is_empty() => Span::styled(
            crate::ui::truncate_end(purpose, purpose_room),
            text.patch(style),
        ),
        _ => Span::styled("(none yet)", dim.patch(style)),
    };
    put(
        b,
        inner.x,
        y,
        by_x.saturating_sub(inner.x),
        vec![Span::styled("purpose   ", dim.patch(style)), purpose],
        style,
    );
    put(
        b,
        by_x,
        y,
        cells(&by),
        vec![Span::styled(by.clone(), dim.patch(style))],
        style,
    );
    render.hits.push((
        Rect::new(inner.x, y, inner.width, 1),
        TeamOverlayHit::EditPurpose,
    ));
    y += 2;

    if team.members.is_empty() && y < inner.bottom() {
        put(
            b,
            inner.x,
            y,
            inner.width,
            vec![Span::styled(
                "no members yet: agents started in this group join",
                dim,
            )],
            bg,
        );
        y += 1;
    }
    for (index, member) in team.members.iter().enumerate() {
        if y >= inner.bottom().saturating_sub(1) {
            break;
        }
        let row = TeamOverlayRow::Member(index);
        let style = row_style(&row);
        let line = Rect::new(inner.x, y, inner.width, 1);
        b.set_style(line, style);
        let remove = button(b, right, y, "×", Style::default().fg(p.red).patch(style));
        render.hits.push((
            remove,
            TeamOverlayHit::Remove {
                pane_id: member.pane_id.clone(),
            },
        ));
        let edit_text = if member.role.is_some() {
            "✎"
        } else {
            "✎ set a role"
        };
        let edit = button(
            b,
            remove.x.saturating_sub(3),
            y,
            edit_text,
            accent.patch(style),
        );
        render.hits.push((
            edit,
            TeamOverlayHit::EditRole {
                pane_id: member.pane_id.clone(),
            },
        ));
        let (mark, name) = match member.role.as_deref() {
            Some(_) => (TEAM_MARK, member_name(member).to_owned()),
            None => (NO_ROLE_MARK, "(no role)".to_owned()),
        };
        let agent = member.agent.as_deref().unwrap_or("no agent");
        let status = match (member.agent.as_deref(), member.status) {
            (None, _) => Vec::new(),
            (Some(_), Some(status)) => {
                let since = member
                    .status_since_unix
                    .map(|since| age(overlay.now_unix.saturating_sub(since)))
                    .unwrap_or_default();
                vec![
                    Span::styled(
                        status_icon(status, status_indicators),
                        Style::default().fg(status_color(status, p)).patch(style),
                    ),
                    Span::styled(format!(" {:<9}", status_text(status)), text.patch(style)),
                    Span::styled(format!("{since:>4}"), dim.patch(style)),
                ]
            }
            (Some(_), None) => vec![Span::styled("…", dim.patch(style))],
        };
        let mut spans = vec![
            Span::styled(
                format!("{mark} "),
                Style::default().fg(p.accent).patch(style),
            ),
            Span::styled(
                format!("{:<12} ", crate::ui::truncate_end(&name, 12)),
                if member.role.is_some() {
                    text.patch(style)
                } else {
                    dim.patch(style)
                },
            ),
            Span::styled(
                format!("{:<8} ", crate::ui::truncate_end(agent, 8)),
                dim.patch(style),
            ),
            Span::styled(
                format!("{:<7} ", crate::ui::truncate_end(&member.pane_id, 7)),
                dim.patch(style),
            ),
        ];
        spans.extend(status);
        put(
            b,
            inner.x,
            y,
            edit.x.saturating_sub(inner.x + 1),
            spans,
            style,
        );
        render.hits.push((
            line,
            TeamOverlayHit::Focus {
                pane_id: member.pane_id.clone(),
            },
        ));
        y += 1;
    }
    for (index, pane_id) in team.excluded.iter().enumerate() {
        if y >= inner.bottom().saturating_sub(1) {
            break;
        }
        let style = row_style(&TeamOverlayRow::Excluded(index));
        b.set_style(Rect::new(inner.x, y, inner.width, 1), style);
        let join = button(b, right, y, "+ join", accent.patch(style));
        render.hits.push((
            join,
            TeamOverlayHit::Join {
                pane_id: pane_id.clone(),
            },
        ));
        put(
            b,
            inner.x,
            y,
            join.x.saturating_sub(inner.x + 1),
            vec![Span::styled(
                format!("  removed: {pane_id}"),
                dim.patch(style),
            )],
            style,
        );
        y += 1;
    }
    let hint_y = inner.bottom().saturating_sub(1);
    if hint_y > y {
        put(
            b,
            inner.x,
            hint_y,
            inner.width,
            vec![Span::styled(
                crate::ui::truncate_end(HINT, usize::from(inner.width)),
                dim,
            )],
            bg,
        );
    }
    Some(render)
}
