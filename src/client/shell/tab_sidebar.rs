//! `ui.sidebar_layout = "tabs"`: the expanded sidebar as one row per tab,
//! with spaces shown as tab groups.
//!
//! Rows follow `snapshot.tabs`, which the endpoint already emits space by
//! space and then in tab order, so the list mirrors tab reordering directly.
//! The FIRST space is the ungrouped bucket and renders without a header; every
//! other space renders as a group: a header row (fold marker, name, member
//! count, rolled-up status) followed by its tab rows unless the group is
//! folded. Fold state is the client's collapsed-group preference keyed by
//! `space:<workspace id>` (`group_key`); the group holding the focused tab is
//! always drawn open, so focus is never hidden.
//!
//! A toolbar row at the top offers fold all / unfold all / new group. Headers
//! also register as `hits.workspaces` so the space drag machinery reorders
//! groups; tab rows register in `hits.sidebar_tabs`. The list reuses the
//! agent panel's scroll state and hit rectangles (`agent_body`,
//! `agent_scrollbar`, `agent_scroll`) so wheel and scrollbar handling need no
//! new plumbing.
//!
//! The horizontal tab bar is hidden in this layout, so its `ui.tab_bar_right`
//! status segments show as dim rows under the list, above the menu row, and
//! only while at least one segment has text. A command entry with `lines > 1`
//! sends its lines joined with `\n` and one with `ansi = true` keeps SGR
//! sequences; the footer draws one row per line (at most
//! `MAX_STATUS_ROWS`) and parses the SGR into styles.
//!
//! The News tab (`news.rs`) is pinned: it leaves the scrolling list and takes
//! one row between the list and the status footer (`hits.news_row`), with a
//! state glyph, `News` and a short status. With news enabled the row stays
//! when there is no News tab (a click creates it).
//!
//! The coordinator tab (`coordinator.rs`) is pinned the same way, as the
//! bottom-most pinned row (Browser, News, coordinator from top to bottom);
//! when not every pinned row fits it keeps its row first, then News, then
//! Browser. Every tab is part of herdr+ (agents model v2): no tab carries a
//! managed mark.
//!
//! Fork, teams (`teams.rs`): a team group's header shows `◆ <purpose>` (the
//! mark in accent), or `◆ <group label>` dim before a purpose exists; a
//! member's row shows a dim `◆` before the agent glyph. Both are O(1)
//! lookups in the client's team maps.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::render::{put_right_text, put_text, render_sidebar_background, ShellRenderState};
use super::*;

const TOOLBAR_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 1;
/// The most `ui.tab_bar_right` status rows between the list and the menu row.
const MAX_STATUS_ROWS: usize = crate::config::MAX_TAB_BAR_COMMAND_LINES as usize;
/// Toolbar glyphs: the fold toggle shows the action it will take.
pub(super) const FOLD_ALL_LABEL: &str = "\u{23F6}"; // ⏶ black medium up-pointing triangle
pub(super) const UNFOLD_ALL_LABEL: &str = "\u{23F7}"; // ⏷ black medium down-pointing triangle
pub(super) const NEW_GROUP_LABEL: &str = "+";

/// The space holding the focused tab; its group is always drawn open.
fn focused_workspace(snapshot: &ClientShellSnapshot) -> Option<&str> {
    snapshot
        .tabs
        .iter()
        .find(|tab| tab.focused)
        .map(|tab| tab.workspace_id.as_str())
}

/// Fold keys of every group that can actually fold: all groups except the one
/// holding the focused tab.
pub(super) fn foldable_group_keys(
    snapshot: &ClientShellSnapshot,
) -> impl Iterator<Item = String> + '_ {
    let focused = focused_workspace(snapshot);
    snapshot
        .workspaces
        .iter()
        .enumerate()
        .filter(move |(index, workspace)| {
            is_group_index(*index) && focused != Some(workspace.workspace_id.as_str())
        })
        .map(|(_, workspace)| group_key(&workspace.workspace_id))
}

/// `Some(true)` when every foldable group is folded (the toggle expands),
/// `Some(false)` when at least one is open, `None` without foldable groups.
pub(super) fn all_groups_folded(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Option<bool> {
    let mut keys = foldable_group_keys(snapshot).peekable();
    keys.peek()?;
    Some(keys.all(|key| collapsed_groups.contains(&key)))
}

/// One row of the tab list.
enum Entry<'a> {
    Header {
        workspace: &'a crate::protocol::ClientShellWorkspace,
        folded: bool,
        members: usize,
    },
    Tab(&'a crate::protocol::ClientShellTab),
}

/// Whether the space at `index` is a group (everything but the first space).
pub(super) fn is_group_index(index: usize) -> bool {
    index > 0
}

/// The rows to draw, honouring fold state except for the focused tab's group.
/// One pass over the tabs, which the endpoint emits space by space. The
/// pinned tabs (News, coordinator: `pinned_tab_ids`) are left out.
fn entries<'a>(
    snapshot: &'a ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
    pinned_tab_ids: &[&str],
) -> Vec<Entry<'a>> {
    let focused = focused_workspace(snapshot);
    let position: HashMap<&str, usize> = snapshot
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| (workspace.workspace_id.as_str(), index))
        .collect();
    let mut members: Vec<Vec<&crate::protocol::ClientShellTab>> =
        vec![Vec::new(); snapshot.workspaces.len()];
    for tab in &snapshot.tabs {
        if pinned_tab_ids.contains(&tab.tab_id.as_str()) {
            continue;
        }
        if let Some(index) = position.get(tab.workspace_id.as_str()) {
            members[*index].push(tab);
        }
    }
    let mut rows = Vec::with_capacity(snapshot.tabs.len() + snapshot.workspaces.len());
    for (index, (workspace, tabs)) in snapshot.workspaces.iter().zip(members).enumerate() {
        let mut folded = false;
        if is_group_index(index) {
            folded = focused != Some(workspace.workspace_id.as_str())
                && collapsed_groups.contains(&group_key(&workspace.workspace_id));
            rows.push(Entry::Header {
                workspace,
                folded,
                members: tabs.len(),
            });
        }
        if !folded {
            rows.extend(tabs.into_iter().map(Entry::Tab));
        }
    }
    rows
}

/// The coordinator's part of the `tabs` sidebar: its pinned row
/// (`coordinator.rs`), and the endpoint's teams. Empty without either.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TabSidebarCoordinator<'a> {
    pub(super) row: Option<&'a super::coordinator::CoordinatorRow>,
    /// Fork: the endpoint's teams (header marks, member rows).
    pub(super) teams: Option<&'a super::teams::ClientTeamsState>,
}

/// The `tabs` sidebar, with the coordinator's row and team marks.
/// Returns the coordinator row's rect (empty when it was not drawn), the
/// hit area of a click on it.
pub(super) fn render_tab_sidebar_with(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
    coordinator: TabSidebarCoordinator<'_>,
) -> Rect {
    let mut coordinator_rect = Rect::default();
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    hits.sidebar_divider = if area.is_empty() {
        Rect::default()
    } else {
        Rect::new(area.right().saturating_sub(1), area.y, 1, area.height)
    };
    hits.sidebar_section_divider = Rect::default();
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.is_empty() {
        return coordinator_rect;
    }

    render_toolbar(
        buffer,
        content,
        all_groups_folded(snapshot, state.collapsed_groups),
        config,
        hits,
    );

    let status_lines = status_footer_lines(snapshot);
    // Rows left under the toolbar and above the menu row; the status keeps
    // one of them for the list once it has more than one line, and the
    // pinned rows (Browser, News, coordinator) take one each while they show.
    let available = content.height.saturating_sub(TOOLBAR_ROWS + FOOTER_ROWS);
    let pinned: Vec<PinnedRow<'_>> = state
        .browser_row
        .as_ref()
        .map(PinnedRow::Browser)
        .into_iter()
        .chain(state.news_row.as_ref().map(PinnedRow::News))
        .chain(coordinator.row.map(PinnedRow::Coordinator))
        .collect();
    let pinned = pinned_rows_that_fit(pinned, available.saturating_sub(1));
    let news_rows = pinned.len() as u16;
    let available = available.saturating_sub(news_rows);
    let status_rows = (status_lines.len().min(usize::from(u16::MAX)) as u16)
        .min(available.saturating_sub(1).max(1))
        .min(available);
    let body = Rect::new(
        content.x,
        content.y.saturating_add(TOOLBAR_ROWS),
        content.width,
        content
            .height
            .saturating_sub(TOOLBAR_ROWS + FOOTER_ROWS + status_rows + news_rows),
    );
    // Top-down from the list's bottom edge: Browser, News, coordinator.
    for (offset, row) in pinned.iter().enumerate() {
        let rect = Rect::new(
            content.x,
            body.bottom().saturating_add(offset as u16),
            content.width,
            1,
        );
        match row {
            PinnedRow::Browser(row) => {
                render_browser_row(buffer, rect, row, config);
                hits.browser_row = rect;
            }
            PinnedRow::News(row) => {
                render_news_row(buffer, rect, row, config);
                hits.news_row = rect;
            }
            PinnedRow::Coordinator(row) => {
                render_coordinator_row(buffer, rect, row, config);
                coordinator_rect = rect;
            }
        }
    }
    if status_rows > 0 {
        render_tab_status_footer(
            buffer,
            content,
            body.bottom().saturating_add(news_rows),
            &status_lines[..usize::from(status_rows)],
            palette,
        );
    }
    hits.agent_body = body;
    // The space drag machinery reads these as the list bounds.
    hits.workspace_body = body;
    let footer_y = content.bottom().saturating_sub(1);
    hits.new_workspace = Rect::new(content.x, footer_y, 0, 1);

    // One pass over the agents; rows then look their glyph key, running
    // subagent count and voice mode (fork) up by tab id.
    let mut glyph_keys = std::collections::HashMap::<&str, &str>::new();
    let mut tab_subagents = std::collections::HashMap::<&str, u32>::new();
    // Fork: no lookups while no pane is in voice mode; the map allocates
    // only for a tab that is.
    let voice = state.voice.filter(|voice| !voice.panes.is_empty());
    let mut tab_voice =
        std::collections::HashMap::<&str, crate::api::schema::AgentVoiceMode>::new();
    for agent in &snapshot.agents {
        glyph_keys.insert(
            agent.tab_id.as_str(),
            agent.agent.as_deref().unwrap_or("other"),
        );
        if agent.subagents > 0 && shows_subagents(agent.agent_status) {
            let count = tab_subagents.entry(agent.tab_id.as_str()).or_default();
            *count = count.saturating_add(agent.subagents);
        }
        if let Some(voice) = voice.and_then(|voice| voice.voice_of(&agent.pane_id)) {
            let current = tab_voice.get(agent.tab_id.as_str()).copied();
            if let Some(louder) = super::voice::louder(current, voice) {
                tab_voice.insert(agent.tab_id.as_str(), louder);
            }
        }
    }
    let pinned_tab_ids: Vec<&str> = state
        .news_row
        .as_ref()
        .and_then(|row| row.tab_id.as_deref())
        .into_iter()
        .chain(coordinator.row.and_then(|row| row.tab_id.as_deref()))
        .collect();
    let rows = entries(snapshot, state.collapsed_groups, &pinned_tab_ids);
    let row_heights = vec![1u16; rows.len()];
    let gaps = vec![0u16; rows.len()];
    let mut metrics =
        super::scroll::list_scroll_metrics(&row_heights, &gaps, body.height, *state.agent_scroll);
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = rows
            .iter()
            .position(|row| matches!(row, Entry::Tab(tab) if tab.focused))
        {
            *state.agent_scroll = super::scroll::list_scroll_start_to_reveal(
                &row_heights,
                &gaps,
                body.height,
                *state.agent_scroll,
                target,
            );
            metrics = super::scroll::list_scroll_metrics(
                &row_heights,
                &gaps,
                body.height,
                *state.agent_scroll,
            );
        }
    }
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *state.agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));

    let mut y = body.y;
    for row in rows.iter().skip(*state.agent_scroll) {
        if y >= body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, 1);
        match row {
            Entry::Header {
                workspace,
                folded,
                members,
            } => {
                let dragged = state.dragged_workspace_id == Some(workspace.workspace_id.as_str());
                let team = coordinator
                    .teams
                    .and_then(|teams| teams.team(&workspace.workspace_id));
                render_group_header(
                    buffer, rect, workspace, *folded, *members, dragged, team, config,
                );
                hits.sidebar_groups
                    .push((rect, workspace.workspace_id.clone()));
                hits.workspaces.push(WorkspaceHit {
                    rect,
                    endpoint_id: ClientEndpointId::Local,
                    workspace_id: workspace.workspace_id.clone(),
                    indented: false,
                    group_toggle: None,
                });
            }
            Entry::Tab(tab) => {
                let glyph_key = glyph_keys
                    .get(tab.tab_id.as_str())
                    .copied()
                    .unwrap_or("shell");
                let glyph = crate::config::tab_agent_glyph(&config.tab_agent_glyphs, glyph_key);
                // Only the focused row wears the agent's brand color.
                let glyph_color = tab.focused.then(|| {
                    crate::config::tab_agent_glyph_color(&config.tab_agent_glyph_colors, glyph_key)
                });
                let subagents = tab_subagents.get(tab.tab_id.as_str()).copied().unwrap_or(0);
                let mut markers = reminder_markers(tab, state, &config.palette);
                if coordinator
                    .teams
                    .is_some_and(|teams| teams.is_member_tab(&tab.tab_id))
                {
                    // A member's dim mark leads the markers.
                    markers.insert(0, (super::teams::TEAM_MARK, config.palette.overlay0));
                }
                // Fork: an agent in voice mode leads them all (red while it
                // listens, dim while muted).
                if let Some(voice) = tab_voice.get(tab.tab_id.as_str()) {
                    markers.insert(0, super::voice::voice_mark(*voice, &config.palette));
                }
                // Fork: a working tab's glyph breathes (`breathe.rs`).
                let breathe = (tab.agent_status == crate::api::schema::AgentStatus::Working
                    && !glyph.is_empty())
                .then_some((state.breathe_phase, state.breathe_reset_rgb));
                hits.breathing |= breathe.is_some();
                render_tab_row(
                    buffer,
                    rect,
                    tab,
                    glyph,
                    glyph_color.flatten(),
                    subagents,
                    &markers,
                    breathe,
                    config,
                );
                hits.sidebar_tabs.push((rect, tab.tab_id.clone()));
            }
        }
        y = y.saturating_add(1);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    // Drop indicators: a group being reordered, or a tab being moved.
    let indicator = state
        .workspace_drop_indicator_row
        .or(state.sidebar_tab_drop_row)
        .filter(|row| *row >= body.y && *row < body.bottom());
    if let Some(row) = indicator {
        put_text(
            buffer,
            body.x,
            row,
            content_width,
            &"─".repeat(content_width as usize),
            Style::default().fg(palette.accent),
        );
    }

    if config.mouse_capture && content.height > TOOLBAR_ROWS {
        let attention = super::global_menu::global_menu_attention(snapshot);
        let launcher_width = if attention { 8 } else { 6 }.min(content.width);
        hits.global_launcher = Rect::new(
            content.right().saturating_sub(launcher_width),
            footer_y,
            launcher_width,
            1,
        );
        if attention {
            let start_x = content.right().saturating_sub(6);
            put_text(
                buffer,
                start_x,
                footer_y,
                2,
                "● ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            put_text(
                buffer,
                start_x.saturating_add(2),
                footer_y,
                4,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        } else {
            put_right_text(
                buffer,
                content,
                footer_y,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        }
    }

    hits.sidebar_toggle = Rect::new(
        area.right().saturating_sub(2),
        area.bottom().saturating_sub(1),
        u16::from(area.width > 1),
        u16::from(area.height > 0),
    );
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "«",
        Style::default().fg(palette.overlay0),
    );
    coordinator_rect
}

/// The pinned rows under the list, in drawing order.
#[derive(Debug, Clone, Copy)]
pub(super) enum PinnedRow<'a> {
    Browser(&'a super::browser::BrowserRow),
    News(&'a super::news::NewsRow),
    Coordinator(&'a super::coordinator::CoordinatorRow),
}

impl PinnedRow<'_> {
    /// Which row keeps its place when not all fit: lower goes first.
    fn keep_rank(&self) -> u8 {
        match self {
            Self::Coordinator(_) => 0,
            Self::News(_) => 1,
            Self::Browser(_) => 2,
        }
    }
}

/// The pinned rows to draw in `room` rows, still in drawing order (Browser,
/// News, coordinator). When not every row fits, the coordinator keeps its
/// row first, then News, then Browser.
pub(super) fn pinned_rows_that_fit(rows: Vec<PinnedRow<'_>>, room: u16) -> Vec<PinnedRow<'_>> {
    let room = usize::from(room);
    if rows.len() <= room {
        return rows;
    }
    let mut ranks: Vec<u8> = rows.iter().map(PinnedRow::keep_rank).collect();
    ranks.sort_unstable();
    let cutoff = ranks.get(room).copied().unwrap_or(u8::MAX);
    rows.into_iter()
        .filter(|row| row.keep_rank() < cutoff)
        .collect()
}

/// The pinned Browser row: ` <glyph> Browser … <status> `, the glyph lit
/// (accent) while an agent uses a tab, the status dim and right-aligned.
fn render_browser_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &super::browser::BrowserRow,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let glyph = row.state.glyph();
    let lead = 1 + display_width(glyph) as u16 + 1;
    // The label wins over the status in a narrow sidebar: the status is
    // truncated first, the label only when nothing else is left.
    let label_width = display_width(super::browser::BROWSER_ROW_LABEL) as u16;
    let status_room = rect.width.saturating_sub(lead + label_width + 2) as usize;
    let status = crate::ui::truncate_end(&row.status, status_room);
    let status_cells = display_width(&status) as u16 + 1;
    let available = rect.width.saturating_sub(lead + status_cells + 1) as usize;
    let label = crate::ui::truncate_end(super::browser::BROWSER_ROW_LABEL, available);
    let pad = rect
        .width
        .saturating_sub(lead + display_width(&label) as u16 + status_cells);
    let spans = vec![
        Span::raw(" "),
        Span::styled(
            glyph,
            Style::default().fg(row.state.color(palette, row.active)),
        ),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(palette.subtext0)),
        Span::raw(" ".repeat(usize::from(pad))),
        Span::styled(status, Style::default().fg(palette.overlay1)),
        Span::raw(" "),
    ];
    Paragraph::new(Line::from(spans)).render(rect, buffer);
}

/// The pinned News row: ` <glyph> News … <status> `, the label bold on the
/// focused tab's row (with the focused row background), the glyph in the
/// state's color, the status dim and right-aligned.
fn render_news_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &super::news::NewsRow,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let label_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.subtext0)
    };
    let glyph = row.state.glyph();
    let lead = 1 + display_width(glyph) as u16 + 1;
    let status_cells = display_width(&row.status) as u16 + 1;
    let available = rect.width.saturating_sub(lead + status_cells + 1) as usize;
    let label = crate::ui::truncate_end(super::news::NEWS_ROW_LABEL, available);
    let pad = rect
        .width
        .saturating_sub(lead + display_width(&label) as u16 + status_cells);
    let spans = vec![
        Span::raw(" "),
        Span::styled(glyph, Style::default().fg(row.state.color(palette))),
        Span::raw(" "),
        Span::styled(label, label_style),
        Span::raw(" ".repeat(usize::from(pad))),
        Span::styled(row.status.clone(), Style::default().fg(palette.overlay1)),
        Span::raw(" "),
    ];
    Paragraph::new(Line::from(spans))
        .style(row_style)
        .render(rect, buffer);
}

/// The pinned coordinator row: ` <glyph> coordinator … <status> `, styled
/// as the News row (bold label and row background while focused, the glyph
/// in the state's color, the status dim and right-aligned).
fn render_coordinator_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &super::coordinator::CoordinatorRow,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let label_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.subtext0)
    };
    let glyph = row.state.glyph();
    let lead = 1 + display_width(glyph) as u16 + 1;
    // The label wins over the status in a narrow sidebar, as on the
    // Browser row.
    let label_width = display_width(super::coordinator::COORDINATOR_ROW_LABEL) as u16;
    let status_room = rect.width.saturating_sub(lead + label_width + 2) as usize;
    let status = crate::ui::truncate_end(&row.status, status_room);
    let status_cells = display_width(&status) as u16 + 1;
    let available = rect.width.saturating_sub(lead + status_cells + 1) as usize;
    let label = crate::ui::truncate_end(super::coordinator::COORDINATOR_ROW_LABEL, available);
    let pad = rect
        .width
        .saturating_sub(lead + display_width(&label) as u16 + status_cells);
    let spans = vec![
        Span::raw(" "),
        Span::styled(glyph, Style::default().fg(row.state.color(palette))),
        Span::raw(" "),
        Span::styled(label, label_style),
        Span::raw(" ".repeat(usize::from(pad))),
        Span::styled(status, Style::default().fg(palette.overlay1)),
        Span::raw(" "),
    ];
    Paragraph::new(Line::from(spans))
        .style(row_style)
        .render(rect, buffer);
}

/// The footer's lines: the non-empty status segments joined with the
/// configured separator, split on `\n`, visibly empty lines dropped, at most
/// `MAX_STATUS_ROWS`. Empty without any status text.
pub(super) fn status_footer_lines(snapshot: &ClientShellSnapshot) -> Vec<String> {
    if snapshot
        .tab_bar_right
        .iter()
        .all(|segment| segment.text.is_empty())
    {
        return Vec::new();
    }
    snapshot
        .tab_bar_right
        .iter()
        .filter(|segment| !segment.text.is_empty())
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(&snapshot.tab_bar_right_separator)
        .split('\n')
        .filter(|line| !strip_status_escapes(line).trim().is_empty())
        .take(MAX_STATUS_ROWS)
        .map(str::to_owned)
        .collect()
}

/// The status footer, dim, one row per line from `y` down, with the rows'
/// one-cell margins; SGR styles apply over the dim base and each row is
/// truncated from the right with `…`.
fn render_tab_status_footer(
    buffer: &mut Buffer,
    content: Rect,
    y: u16,
    lines: &[String],
    palette: &Palette,
) {
    let width = content.width.saturating_sub(2);
    let base = Style::default().fg(palette.overlay1);
    for (row, line) in lines.iter().enumerate() {
        let spans = truncate_status_spans(styled_status_spans(line, base), usize::from(width));
        put_status_spans(
            buffer,
            content.x.saturating_add(1),
            y.saturating_add(row as u16),
            width,
            &spans,
        );
    }
}

/// A status segment for the single-line `spaces` tab bar: a segment holding
/// `\n` or an escape shows only its last line with escapes stripped, so the
/// upstream bar never draws raw escapes. Other segments pass unchanged.
pub(super) fn single_line_status_text(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(['\n', '\x1b']) {
        return std::borrow::Cow::Borrowed(text);
    }
    let last = text.rsplit('\n').next().unwrap_or_default();
    std::borrow::Cow::Owned(strip_status_escapes(last))
}

/// Removes escape sequences: CSI (`ESC [` up to its final byte), and any
/// other `ESC` with the character after it.
fn strip_status_escapes(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '\x1b' {
            output.push(character);
            continue;
        }
        if chars.next() == Some('[') {
            for sequence in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&sequence) {
                    break;
                }
            }
        }
    }
    output
}

/// Splits one footer line into styled spans. SGR sequences restyle the text
/// after them over `base`: reset (0 or empty), bold (1), dim (2), normal
/// intensity (22), foreground 30–37 / 90–97, `38;5;n`, `38;2;r;g;b` and the
/// default foreground (39). Other parameters and other escapes are dropped.
fn styled_status_spans(line: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut style = base;
    let mut text = String::new();
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\x1b' {
            if !character.is_control() {
                text.push(character);
            }
            continue;
        }
        if chars.next_if_eq(&'[').is_none() {
            chars.next();
            continue;
        }
        let mut params = String::new();
        let mut final_byte = None;
        for sequence in chars.by_ref() {
            if ('\x40'..='\x7e').contains(&sequence) {
                final_byte = Some(sequence);
                break;
            }
            params.push(sequence);
        }
        if final_byte != Some('m') {
            continue;
        }
        let next = apply_sgr(style, base, &params);
        if next != style && !text.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut text), style));
        }
        style = next;
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
    spans
}

fn apply_sgr(mut style: Style, base: Style, params: &str) -> Style {
    if !params
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b';')
    {
        return style;
    }
    let mut codes = params
        .split(';')
        .map(|code| code.parse::<u16>().unwrap_or(0));
    while let Some(code) = codes.next() {
        style = match code {
            0 => base,
            1 => style.add_modifier(Modifier::BOLD),
            2 => style.add_modifier(Modifier::DIM),
            22 => style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            30..=37 => style.fg(basic_color(code - 30, false)),
            90..=97 => style.fg(basic_color(code - 90, true)),
            39 => Style {
                fg: base.fg,
                ..style
            },
            38 | 48 => {
                // Extended colors; 48 (background) is consumed but ignored.
                let color = match codes.next() {
                    Some(5) => codes
                        .next()
                        .and_then(|index| u8::try_from(index).ok())
                        .map(ratatui::style::Color::Indexed),
                    Some(2) => {
                        let mut channel =
                            || codes.next().and_then(|value| u8::try_from(value).ok());
                        match (channel(), channel(), channel()) {
                            (Some(r), Some(g), Some(b)) => {
                                Some(ratatui::style::Color::Rgb(r, g, b))
                            }
                            _ => None,
                        }
                    }
                    _ => return style,
                };
                match color {
                    Some(color) if code == 38 => style.fg(color),
                    _ => style,
                }
            }
            _ => style,
        };
    }
    style
}

fn basic_color(index: u16, bright: bool) -> ratatui::style::Color {
    use ratatui::style::Color;
    match (index, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (7, false) => Color::Gray,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        _ => Color::White,
    }
}

/// Keeps the spans within `width` display cells; when they do not fit the
/// last kept cell is `…` in the style of the text it replaces.
fn truncate_status_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|span| span.width()).sum();
    if total <= width {
        return spans;
    }
    if width == 0 {
        return Vec::new();
    }
    let mut budget = width - 1;
    let mut kept = Vec::new();
    for span in spans {
        let span_width = span.width();
        if span_width <= budget {
            budget -= span_width;
            kept.push(span);
            continue;
        }
        let mut text = String::new();
        for character in span.content.chars() {
            let character_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
            if character_width > budget {
                break;
            }
            budget -= character_width;
            text.push(character);
        }
        text.push('…');
        kept.push(Span::styled(text, span.style));
        return kept;
    }
    kept
}

fn put_status_spans(buffer: &mut Buffer, x: u16, y: u16, width: u16, spans: &[Span<'_>]) {
    let mut x = x;
    let right = x.saturating_add(width);
    for span in spans {
        let span_width =
            (span.width().min(usize::from(u16::MAX)) as u16).min(right.saturating_sub(x));
        put_text(buffer, x, y, span_width, &span.content, span.style);
        x = x.saturating_add(span_width);
    }
}

/// The fold toggle on the left (absent without foldable groups), `+` on the
/// right, all one row.
fn render_toolbar(
    buffer: &mut Buffer,
    content: Rect,
    all_folded: Option<bool>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    let style = Style::default().fg(palette.overlay0);
    let y = content.y;
    let x = content.x.saturating_add(1);
    if let Some(all_folded) = all_folded {
        let toggle = if all_folded {
            UNFOLD_ALL_LABEL
        } else {
            FOLD_ALL_LABEL
        };
        let toggle_width = display_width(toggle) as u16;
        put_text(buffer, x, y, toggle_width, toggle, style);
        if config.mouse_capture {
            hits.group_toggle_all = Rect::new(x, y, toggle_width, 1);
        }
    }
    let new_width = display_width(NEW_GROUP_LABEL) as u16;
    let new_x = content.right().saturating_sub(new_width + 1);
    put_text(buffer, new_x, y, new_width, NEW_GROUP_LABEL, style);
    if config.mouse_capture {
        hits.group_new = Rect::new(new_x, y, new_width, 1);
    }
}

#[allow(clippy::too_many_arguments)] // one row's facts; a struct would only rename them
fn render_group_header(
    buffer: &mut Buffer,
    rect: Rect,
    workspace: &crate::protocol::ClientShellWorkspace,
    folded: bool,
    members: usize,
    dragged: bool,
    team: Option<&crate::api::schema::team::TeamInfo>,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    // Headers are dividers, not items: a quiet band with dim text so the tab
    // rows stay the visually dominant lines.
    let row_style = if dragged {
        Style::default().bg(palette.surface1)
    } else {
        Style::default().bg(palette.surface0)
    };
    let marker = if folded { "▸" } else { "▾" };
    let name_style = Style::default().fg(if workspace.focused {
        palette.subtext0
    } else {
        palette.overlay1
    });
    let count = format!("{members}");
    let status_icon_text = status_icon(workspace.agent_status, config.status_indicators);
    let tail_width = display_width(&count) as u16 + 1 + display_width(status_icon_text) as u16 + 1;
    // Fork: a team group leads with the mark and shows its purpose (the
    // group label, dim, until there is one).
    let team_mark = team.map(|_| super::teams::TEAM_MARK);
    let (text, text_style, mark_style) = match team {
        Some(team) => match super::teams::header_label(team, &workspace.label) {
            (purpose, false) => (purpose, name_style, Style::default().fg(palette.accent)),
            (label, true) => (
                label,
                Style::default().fg(palette.overlay0),
                Style::default().fg(palette.overlay0),
            ),
        },
        None => (workspace.label.as_str(), name_style, name_style),
    };
    let mark_width = team_mark.map_or(0, |mark| display_width(mark) as u16 + 1);
    let lead = 1 + display_width(marker) as u16 + 1 + mark_width;
    let available = rect.width.saturating_sub(lead + tail_width + 1) as usize;
    let label = crate::ui::truncate_end(text, available);
    let pad = rect
        .width
        .saturating_sub(lead + display_width(&label) as u16 + tail_width + 1);
    let dim = Style::default().fg(palette.overlay0);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(marker.to_string(), dim),
        Span::raw(" "),
    ];
    if let Some(mark) = team_mark {
        spans.push(Span::styled(mark, mark_style));
        spans.push(Span::raw(" "));
    }
    spans.extend([
        Span::styled(label, text_style),
        Span::raw(" ".repeat(usize::from(pad) + 1)),
        Span::styled(count, dim),
        Span::raw(" "),
        Span::styled(
            status_icon_text,
            Style::default().fg(status_color(workspace.agent_status, palette)),
        ),
        Span::raw(" "),
    ]);
    Paragraph::new(Line::from(spans))
        .style(row_style)
        .render(rect, buffer);
}

/// A tab row's reminder markers, in order: `★` when the tab is important,
/// the interval's marker (`remind_marker`) when it has a scheduled reminder. Each is overlay0, or the fired
/// reminder's color while it is lit.
fn reminder_markers(
    tab: &crate::protocol::ClientShellTab,
    state: &ShellRenderState<'_>,
    palette: &Palette,
) -> Vec<(&'static str, ratatui::style::Color)> {
    let key = (state.active_endpoint_id.clone(), tab.tab_id.clone());
    let mut markers = Vec::new();
    // A pane of this tab used the herdr browser recently.
    if state.browser_marked_tabs.contains(&tab.tab_id) {
        markers.push((super::browser::TAB_BROWSER_MARKER, palette.accent));
    }
    if tab.important {
        let lit = state
            .idle_reminders
            .get(&key)
            .and_then(|reminder| reminder.lit);
        markers.push((
            TAB_IMPORTANT_MARKER,
            lit.map_or(palette.overlay0, |lit| lit.color(palette)),
        ));
    }
    if let Some(every) = tab
        .remind_every
        .filter(|every| *every != crate::api::schema::TabRemindInterval::Unknown)
    {
        let lit = state
            .scheduled_reminders
            .get(&key)
            .is_some_and(|reminder| reminder.lit);
        markers.push((
            remind_marker(every),
            if lit {
                super::idle_reminders::ClientReminderLit::Scheduled.color(palette)
            } else {
                palette.overlay0
            },
        ));
    }
    markers
}

/// Whether a status gives way to the subagent icon while subagents run: a
/// working, idle or finished agent (background agents outlive the turn).
/// Blocked keeps its icon (needing you outranks), and suspended or unknown
/// agents have none.
fn shows_subagents(status: crate::api::schema::AgentStatus) -> bool {
    use crate::api::schema::AgentStatus;
    matches!(
        status,
        AgentStatus::Working | AgentStatus::Idle | AgentStatus::Done
    )
}

/// The status icon of a tab whose agent has subagents running.
pub(super) const TAB_SUBAGENTS_ICON: &str = "\u{26AD}"; // ⚭ (two interlocking rings)

/// The row marker of an important tab (`tab.set_reminder` important).
pub(super) const TAB_IMPORTANT_MARKER: &str = "\u{2605}"; // ★

/// The row marker of a tab with a minutes-scale scheduled reminder
/// (`tab.set_reminder` every 5m, 10m or 30m), and of an unknown interval.
pub(super) const TAB_REMIND_MARKER: &str = "\u{25F7}"; // ◷
/// The row marker of an hours-scale scheduled reminder (1h, 6h).
pub(super) const TAB_REMIND_HOURS_MARKER: &str = "\u{25D1}"; // ◑
/// The row marker of a daily scheduled reminder.
pub(super) const TAB_REMIND_DAILY_MARKER: &str = "\u{263C}"; // ☼

/// The marker for a scheduled reminder's interval, on the tab row and on its
/// notification card. Only glyphs that render full-size in common terminal
/// fonts: minutes `◷`, hours `◑`, daily `☼`.
pub(super) fn remind_marker(every: crate::api::schema::TabRemindInterval) -> &'static str {
    use crate::api::schema::TabRemindInterval;
    match every {
        TabRemindInterval::H1 | TabRemindInterval::H6 => TAB_REMIND_HOURS_MARKER,
        TabRemindInterval::Daily => TAB_REMIND_DAILY_MARKER,
        TabRemindInterval::M5
        | TabRemindInterval::M10
        | TabRemindInterval::M30
        | TabRemindInterval::Unknown => TAB_REMIND_MARKER,
    }
}

/// A breathing glyph's phase and the terminal default background's RGB.
type Breath = (f32, Option<(u8, u8, u8)>);

fn render_tab_row(
    buffer: &mut Buffer,
    rect: Rect,
    tab: &crate::protocol::ClientShellTab,
    glyph: &str,
    glyph_color: Option<ratatui::style::Color>,
    subagents: u32,
    markers: &[(&str, ratatui::style::Color)],
    breathe: Option<Breath>,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if tab.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    // A color tag only changes the label's foreground; background, bold,
    // the status icon and the glyph keep their own styling.
    let tag_fg = super::tab_color::tab_label_fg(tab.color, palette);
    let label_style = if tab.focused {
        Style::default()
            .fg(tag_fg.unwrap_or(palette.text))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(tag_fg.unwrap_or(palette.subtext0))
    };
    let icon_style = Style::default().fg(status_color(tab.agent_status, palette));
    // An agent with Claude Code subagents running (in the background too)
    // shows the subagent icon in its status color, in either indicator style:
    // yellow working, green idle, teal finished.
    let icon = if shows_subagents(tab.agent_status) && subagents > 0 {
        TAB_SUBAGENTS_ICON
    } else {
        status_icon(tab.agent_status, config.status_indicators)
    };
    // " <icon> <label>...<glyph> ": the agent glyph is right-aligned with a one
    // cell margin and the label gives way to it. A tab with reminders shows
    // their markers just before the glyph.
    let glyph_width = display_width(glyph) as u16;
    let glyph_cells = if glyph_width > 0 { glyph_width + 2 } else { 0 };
    // Each marker is followed by a space; without a glyph, the last one's
    // space is the margin and one more cell separates them from the label.
    let remind_cells = if markers.is_empty() {
        0
    } else {
        markers
            .iter()
            .map(|(marker, _)| display_width(marker) as u16 + 1)
            .sum::<u16>()
            + u16::from(glyph_cells == 0)
    };
    let lead = 1 + display_width(icon) as u16 + 1;
    let available = rect.width.saturating_sub(lead + glyph_cells + remind_cells) as usize;
    let label = crate::ui::truncate_end(&tab.label, available);
    let pad = rect
        .width
        .saturating_sub(lead + display_width(&label) as u16 + glyph_cells + remind_cells);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(icon, icon_style),
        Span::raw(" "),
        Span::styled(label, label_style),
    ];
    if remind_cells > 0 {
        spans.push(Span::raw(" ".repeat(usize::from(pad) + 1)));
        for (marker, color) in markers {
            spans.push(Span::styled(*marker, Style::default().fg(*color)));
            spans.push(Span::raw(" "));
        }
    }
    if glyph_cells > 0 {
        let gap = if remind_cells > 0 {
            0
        } else {
            usize::from(pad) + 1
        };
        spans.push(Span::raw(" ".repeat(gap)));
        let normal = glyph_color.unwrap_or(palette.overlay0);
        let fg = match breathe {
            Some((phase, reset)) => {
                let background = if tab.focused {
                    palette.active_row_bg
                } else {
                    palette.sidebar_bg
                };
                super::breathe::glyph_color(normal, background, reset, phase)
            }
            None => normal,
        };
        spans.push(Span::styled(glyph.to_string(), Style::default().fg(fg)));
        spans.push(Span::raw(" "));
    }
    Paragraph::new(Line::from(spans))
        .style(row_style)
        .render(rect, buffer);
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}
