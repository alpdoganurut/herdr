//! `ui.sidebar_layout = "tabs"`: the expanded sidebar as one row per tab.
//!
//! Rows follow `snapshot.tabs`, which the endpoint already emits space by
//! space and then in tab order, so the list mirrors tab reordering directly.
//! Each row shows the tab's aggregate agent status and its display label.
//! The list reuses the agent panel's scroll state and hit rectangles
//! (`agent_body`, `agent_scrollbar`, `agent_scroll`) so wheel and scrollbar
//! handling need no new plumbing; rows register in `hits.sidebar_tabs`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::render::{put_right_text, put_text, render_sidebar_background, ShellRenderState};
use super::*;

const HEADER_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 1;

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
    put_text(
        buffer,
        content.x,
        content.y,
        content.width,
        " tabs",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );

    let body = Rect::new(
        content.x,
        content.y.saturating_add(HEADER_ROWS),
        content.width,
        content.height.saturating_sub(HEADER_ROWS + FOOTER_ROWS),
    );
    hits.agent_body = body;
    let tabs = &snapshot.tabs;
    let row_heights = vec![1u16; tabs.len()];
    let gaps = vec![0u16; tabs.len()];
    let mut metrics =
        super::scroll::list_scroll_metrics(&row_heights, &gaps, body.height, *state.agent_scroll);
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = tabs.iter().position(|tab| tab.focused) {
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
    for tab in tabs.iter().skip(*state.agent_scroll) {
        if y >= body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, 1);
        render_tab_row(buffer, rect, tab, config);
        hits.sidebar_tabs.push((rect, tab.tab_id.clone()));
        y = y.saturating_add(1);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    let footer_y = content.bottom().saturating_sub(1);
    if config.mouse_capture && content.height > HEADER_ROWS {
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

fn render_tab_row(
    buffer: &mut Buffer,
    rect: Rect,
    tab: &crate::protocol::ClientShellTab,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if tab.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let label_style = if tab.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.subtext0)
    };
    let icon_style = Style::default().fg(status_color(tab.agent_status, palette));
    let icon = status_icon(tab.agent_status, config.status_indicators);
    let available = rect
        .width
        .saturating_sub(1 + display_width(icon) as u16 + 1) as usize;
    let label = crate::ui::truncate_end(&tab.label, available);
    let spans = vec![
        Span::raw(" "),
        Span::styled(icon, icon_style),
        Span::raw(" "),
        Span::styled(label, label_style),
    ];
    Paragraph::new(Line::from(spans))
        .style(row_style)
        .render(rect, buffer);
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}
