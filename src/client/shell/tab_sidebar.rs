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
/// One pass over the tabs, which the endpoint emits space by space.
fn entries<'a>(
    snapshot: &'a ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
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

pub(super) fn render_tab_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
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
        return;
    }

    render_toolbar(
        buffer,
        content,
        all_groups_folded(snapshot, state.collapsed_groups),
        config,
        hits,
    );

    let body = Rect::new(
        content.x,
        content.y.saturating_add(TOOLBAR_ROWS),
        content.width,
        content.height.saturating_sub(TOOLBAR_ROWS + FOOTER_ROWS),
    );
    hits.agent_body = body;
    // The space drag machinery reads these as the list bounds.
    hits.workspace_body = body;
    let footer_y = content.bottom().saturating_sub(1);
    hits.new_workspace = Rect::new(content.x, footer_y, 0, 1);

    // One pass over the agents; rows then look their glyph key up by tab id.
    let glyph_keys: std::collections::HashMap<&str, &str> = snapshot
        .agents
        .iter()
        .map(|agent| {
            (
                agent.tab_id.as_str(),
                agent.agent.as_deref().unwrap_or("other"),
            )
        })
        .collect();
    let rows = entries(snapshot, state.collapsed_groups);
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
                render_group_header(buffer, rect, workspace, *folded, *members, dragged, config);
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
                render_tab_row(buffer, rect, tab, glyph, glyph_color.flatten(), config);
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

fn render_group_header(
    buffer: &mut Buffer,
    rect: Rect,
    workspace: &crate::protocol::ClientShellWorkspace,
    folded: bool,
    members: usize,
    dragged: bool,
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
    let lead = 1 + display_width(marker) as u16 + 1;
    let available = rect.width.saturating_sub(lead + tail_width + 1) as usize;
    let label = crate::ui::truncate_end(&workspace.label, available);
    let pad = rect
        .width
        .saturating_sub(lead + display_width(&label) as u16 + tail_width + 1);
    let dim = Style::default().fg(palette.overlay0);
    let spans = vec![
        Span::raw(" "),
        Span::styled(marker.to_string(), dim),
        Span::raw(" "),
        Span::styled(label, name_style),
        Span::raw(" ".repeat(usize::from(pad) + 1)),
        Span::styled(count, dim),
        Span::raw(" "),
        Span::styled(
            status_icon_text,
            Style::default().fg(status_color(workspace.agent_status, palette)),
        ),
        Span::raw(" "),
    ];
    Paragraph::new(Line::from(spans))
        .style(row_style)
        .render(rect, buffer);
}

/// The monochrome marker on a tab row marked for idle reminders (`tab.set_remind`).
pub(super) const TAB_REMIND_MARKER: &str = "\u{25F7}"; // ◷

fn render_tab_row(
    buffer: &mut Buffer,
    rect: Rect,
    tab: &crate::protocol::ClientShellTab,
    glyph: &str,
    glyph_color: Option<ratatui::style::Color>,
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
    let icon = status_icon(tab.agent_status, config.status_indicators);
    // " <icon> <label>...<glyph> ": the agent glyph is right-aligned with a one
    // cell margin and the label gives way to it. A tab marked for idle
    // reminders shows the reminder marker just before the glyph.
    let glyph_width = display_width(glyph) as u16;
    let glyph_cells = if glyph_width > 0 { glyph_width + 2 } else { 0 };
    // One space before the marker; without a glyph, one margin cell after it.
    let remind_cells = if tab.remind {
        display_width(TAB_REMIND_MARKER) as u16 + 1 + u16::from(glyph_cells == 0)
    } else {
        0
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
        spans.push(Span::styled(
            TAB_REMIND_MARKER,
            Style::default().fg(palette.overlay0),
        ));
        spans.push(Span::raw(" "));
    }
    if glyph_cells > 0 {
        let gap = if remind_cells > 0 {
            0
        } else {
            usize::from(pad) + 1
        };
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(
            glyph.to_string(),
            Style::default().fg(glyph_color.unwrap_or(palette.overlay0)),
        ));
        spans.push(Span::raw(" "));
    }
    Paragraph::new(Line::from(spans))
        .style(row_style)
        .render(rect, buffer);
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}
