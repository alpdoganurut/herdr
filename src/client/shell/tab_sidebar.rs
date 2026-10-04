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
//! `MAX_STATUS_ROWS`) and parses the SGR into styles. Below
//! `SPACIOUS_HEIGHT` rows the lines share one row, two spaces apart.
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
//! Fork, teams (`teams.rs`): a team group's header shows `◆` in its icon
//! slot and its purpose as the name (the mark in accent), or the group label
//! dim before a purpose exists. Member rows carry no mark. Both are O(1)
//! lookups in the client's team maps.
//!
//! Fork, sidebar v2 (the rows). Columns, from the content's left edge: the
//! hover bar `▎` at 0; a header's fold marker at 1, its icon slot (team `◆`)
//! at 3 and its name at 5; a tab's status icon at 5, its voice mark at 7 and
//! its label at 7 (9 after a voice mark). Headers have no band (only while
//! dragged). A tab row's marks (browser `◎`, `★`, the reminder) are packed
//! flush right in that order with a one-cell margin, stride 2 (1 when the
//! label would get fewer than `MIN_LABEL_CELLS`); the selected row (the
//! hovered one, else the focused tab: `sidebar_model::resolve_selected`)
//! adds the agent's harness glyph last, in its brand color, breathing while
//! the tab works. From `SPACIOUS_HEIGHT` rows a blank spacer row sits before
//! every header with a row above it (`SidebarModel::gaps_after`, so scroll
//! math counts it). The chrome rows (toolbar, Active agents block, detail
//! strip, status footer, menu row) sit on `Palette::sidebar_chrome()`; the
//! list and pinned rows keep `sidebar_bg`, the divider is `surface1`.

use std::fmt::Write as _;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
};

use super::render::{
    put_right_text, put_text, put_truncated, render_sidebar_background, ShellRenderState,
};
use super::sidebar_model::{
    resolve_selected, PinnedKind, Row, Selected, SidebarHover, SidebarModel, StackStr, TabFacts,
};
use super::*;

const TOOLBAR_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 1;
/// The most `ui.tab_bar_right` status rows between the list and the menu row.
const MAX_STATUS_ROWS: usize = crate::config::MAX_TAB_BAR_COMMAND_LINES as usize;
/// From this sidebar content height the status footer gets one row per line
/// and spacer rows separate the groups; below it the footer lines share one
/// row.
pub(super) const SPACIOUS_HEIGHT: u16 = 30;
/// The list keeps this many rows while the optional blocks shrink
/// (`plan_layout`).
pub(super) const MIN_LIST_ROWS: u16 = 3;
/// The Active agents block's line caps tried in turn while the list is short.
const ACTIVE_CAP_STEPS: [u16; 3] = [6, 4, 2];
/// A tab label narrower than this packs the marks without gaps.
const MIN_LABEL_CELLS: u16 = 8;
/// The hovered row's bar (accent), at the content's left edge.
pub(super) const HOVER_BAR: &str = "\u{258E}"; // ▎
/// A drop indicator's cell.
const RULE: &str = "\u{2500}"; // ─
/// Toolbar glyphs: the fold toggle shows the action it will take.
pub(super) const FOLD_ALL_LABEL: &str = "\u{23F6}"; // ⏶ black medium up-pointing triangle
pub(super) const UNFOLD_ALL_LABEL: &str = "\u{23F7}"; // ⏷ black medium down-pointing triangle
pub(super) const NEW_GROUP_LABEL: &str = "+";
/// A group header's fold marker.
const GROUP_OPEN_MARKER: &str = "\u{25BE}"; // ▾
const GROUP_FOLDED_MARKER: &str = "\u{25B8}"; // ▸

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

/// Whether the space at `index` is a group (everything but the first space).
pub(super) fn is_group_index(index: usize) -> bool {
    index > 0
}

/// The coordinator's part of the `tabs` sidebar: its pinned row
/// (`coordinator.rs`), and the endpoint's teams. Empty without either.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TabSidebarCoordinator<'a> {
    pub(super) row: Option<&'a super::coordinator::CoordinatorRow>,
    /// Fork: the endpoint's teams (the header marks).
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
    // Fork (sidebar v2): the divider in surface1 on the list's background
    // (a surface_dim one would vanish next to the chrome rows).
    let divider = Style::default().fg(palette.surface1).bg(palette.sidebar_bg);
    for y in hits.sidebar_divider.y..hits.sidebar_divider.bottom() {
        if let Some(cell) = buffer.cell_mut((hits.sidebar_divider.x, y)) {
            cell.set_style(divider);
        }
    }
    hits.sidebar_section_divider = Rect::default();
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.is_empty() {
        return coordinator_rect;
    }

    let status_lines = status_footer_lines(snapshot);
    let pinned: Vec<PinnedRow<'_>> = state
        .browser_row
        .as_ref()
        .map(PinnedRow::Browser)
        .into_iter()
        .chain(state.news_row.as_ref().map(PinnedRow::News))
        .chain(coordinator.row.map(PinnedRow::Coordinator))
        .collect();
    // The pinned tabs (News, coordinator) leave the list.
    let pinned_tab_ids = (
        state
            .news_row
            .as_ref()
            .and_then(|row| row.tab_id.as_deref()),
        coordinator.row.and_then(|row| row.tab_id.as_deref()),
    );
    // Fork (sidebar v2): rows and per-tab facts come from the model compose
    // ensured; a caller without one (tests) gets a one-off build.
    let one_off;
    let model = match state.sidebar_model {
        Some(model) => model,
        None => {
            one_off = SidebarModel::built(
                snapshot,
                state.collapsed_groups,
                state.voice,
                pinned_tab_ids,
                config.sidebar_active_agents,
            );
            &one_off
        }
    };
    let active_view = state.active_view;
    let plan = plan_layout(
        content,
        LayoutWants {
            pinned: u16::try_from(pinned.len()).unwrap_or(u16::MAX),
            status_lines: status_lines.len(),
            active_cap: super::tab_sidebar_active::active_cap(content.height),
            detail_lines: super::tab_sidebar_detail::detail_lines(content.height),
        },
        |cap| super::tab_sidebar_active::active_block_rows(model, active_view, cap),
    );

    // The chrome rows first; the blocks draw on top of their background.
    let chrome = Style::default().bg(palette.sidebar_chrome());
    for rect in [
        plan.toolbar,
        plan.active,
        plan.detail,
        plan.footer,
        plan.menu,
    ] {
        buffer.set_style(rect, chrome);
    }
    render_toolbar(
        buffer,
        content,
        all_groups_folded(snapshot, state.collapsed_groups),
        config,
        hits,
    );

    let hover = state.sidebar_hover;
    let selected = resolve_selected(snapshot, model, hover);
    let pinned = pinned_rows_that_fit(pinned, plan.pinned.height);
    // Top-down from the list's bottom edge: Browser, News, coordinator.
    for (offset, row) in pinned.iter().enumerate() {
        let rect = Rect::new(
            content.x,
            plan.pinned.y.saturating_add(offset as u16),
            content.width,
            1,
        );
        let kind = row.kind();
        let hovered = matches!(hover, Some(SidebarHover::Pinned(hovered)) if *hovered == kind);
        render_pinned_row(buffer, rect, row, hovered, config);
        match row {
            PinnedRow::Browser(_) => hits.browser_row = rect,
            PinnedRow::News(_) => hits.news_row = rect,
            PinnedRow::Coordinator(_) => coordinator_rect = rect,
        }
    }
    if !plan.active.is_empty() {
        super::tab_sidebar_active::render_active_block(
            buffer,
            plan.active,
            snapshot,
            model,
            active_view,
            config,
            state,
            hits,
        );
    }
    if !plan.detail.is_empty() {
        super::tab_sidebar_detail::render_detail_strip(
            buffer,
            plan.detail,
            snapshot,
            model,
            config,
            state,
            hits,
        );
    }
    if plan.footer.height > 0 {
        render_tab_status_footer(buffer, plan.footer, &status_lines, palette);
    }
    let body = plan.list;
    hits.agent_body = body;
    // The space drag machinery reads these as the list bounds.
    hits.workspace_body = body;
    let footer_y = plan.menu.y;
    hits.new_workspace = Rect::new(content.x, footer_y, 0, 1);

    let rows = &model.rows;
    let row_heights = &model.row_heights;
    // Spacer rows before the group headers once there is room for them.
    let gaps = if content.height >= SPACIOUS_HEIGHT {
        &model.gaps_after
    } else {
        &model.no_gaps
    };
    let list_tab = |row: &Row| match row {
        Row::Tab { tab } => snapshot.tabs.get(*tab as usize),
        Row::Header { .. } => None,
    };
    let mut metrics =
        super::scroll::list_scroll_metrics(row_heights, gaps, body.height, *state.agent_scroll);
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = rows
            .iter()
            .position(|row| list_tab(row).is_some_and(|tab| tab.focused))
        {
            *state.agent_scroll = super::scroll::list_scroll_start_to_reveal(
                row_heights,
                gaps,
                body.height,
                *state.agent_scroll,
                target,
            );
            metrics = super::scroll::list_scroll_metrics(
                row_heights,
                gaps,
                body.height,
                *state.agent_scroll,
            );
        }
    }
    // Fork (sidebar v2): a tab named by id (an Active agents jump) is
    // revealed before the server's focus change lands.
    if !body.is_empty() {
        if let Some(tab_id) = state.sidebar_reveal_tab.take() {
            if let Some(target) = rows
                .iter()
                .position(|row| list_tab(row).is_some_and(|tab| tab.tab_id == tab_id))
            {
                *state.agent_scroll = super::scroll::list_scroll_start_to_reveal(
                    row_heights,
                    gaps,
                    body.height,
                    *state.agent_scroll,
                    target,
                );
                metrics = super::scroll::list_scroll_metrics(
                    row_heights,
                    gaps,
                    body.height,
                    *state.agent_scroll,
                );
            }
        }
    }
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *state.agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));

    // The selected tab (hovered, else focused) shows its harness glyph; a
    // hovered row also gets the bar and the band.
    let selected_tab = match selected {
        Selected::Tab(index) | Selected::ActiveEntry(index) => Some(index),
        _ => None,
    };
    let hovered_group = match selected {
        Selected::Group(index) => Some(index),
        _ => None,
    };
    let hovered_tab_id = match hover {
        Some(SidebarHover::Tab(tab_id)) => Some(tab_id.as_str()),
        _ => None,
    };
    let mut y = body.y;
    for (index, row) in rows.iter().enumerate().skip(*state.agent_scroll) {
        if y >= body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, 1);
        y = y
            .saturating_add(1)
            .saturating_add(gaps.get(index).copied().unwrap_or(0));
        match *row {
            Row::Header {
                workspace: workspace_index,
                folded,
                members,
            } => {
                let Some(workspace) = snapshot.workspaces.get(workspace_index as usize) else {
                    continue;
                };
                let dragged = state.dragged_workspace_id == Some(workspace.workspace_id.as_str());
                let team = coordinator
                    .teams
                    .and_then(|teams| teams.team(&workspace.workspace_id));
                render_group_header(
                    buffer,
                    rect,
                    workspace,
                    HeaderLook {
                        folded,
                        members,
                        dragged,
                        hovered: hovered_group == Some(workspace_index),
                    },
                    team,
                    config,
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
            Row::Tab { tab: tab_index } => {
                let (Some(tab), Some(facts)) =
                    (snapshot.tabs.get(tab_index as usize), model.tab(tab_index))
                else {
                    continue;
                };
                let hovered = hovered_tab_id == Some(tab.tab_id.as_str());
                let background = if hovered {
                    palette.sidebar_hover_bg()
                } else if tab.focused {
                    palette.active_row_bg
                } else {
                    palette.sidebar_bg
                };
                let mut marks = Marks::default();
                push_reminder_marks(&mut marks, tab, state, palette);
                if selected_tab == Some(tab_index) {
                    if let Some(harness) =
                        harness_mark(snapshot, tab, facts, background, state, config)
                    {
                        hits.breathing |= harness.breathing;
                        marks.push(harness.glyph, harness.color);
                    }
                }
                let voice = facts
                    .voice
                    .map(|voice| super::voice::voice_mark(voice, config));
                render_tab_row(
                    buffer,
                    rect,
                    tab,
                    facts.subagents,
                    TabRowLook {
                        hovered,
                        background: (hovered || tab.focused).then_some(background),
                        voice,
                        marks: marks.as_slice(),
                    },
                    config,
                );
                hits.sidebar_tabs.push((rect, tab.tab_id.clone()));
            }
        }
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
        let style = Style::default().fg(palette.accent);
        for x in body.x..body.x.saturating_add(content_width) {
            put_text(buffer, x, row, 1, RULE, style);
        }
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

/// Fork (sidebar v2): the tabs sidebar's rows, top to bottom (`plan_layout`).
/// Every rect spans the content width. `toolbar`, `active`, `detail`,
/// `footer` and `menu` are chrome rows (painted with
/// `Palette::sidebar_chrome()` before anything draws on them).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SidebarPlan {
    pub(super) toolbar: Rect,
    /// The Active agents block (`tab_sidebar_active.rs`): exactly the rows
    /// `active_block_rows` asked for at the cap the layout settled on
    /// (header, lines, `+N more`, rule), or empty.
    pub(super) active: Rect,
    /// The scrolling list.
    pub(super) list: Rect,
    /// The pinned rows (Browser, News, coordinator).
    pub(super) pinned: Rect,
    /// The detail strip (`tab_sidebar_detail.rs`): its upper rule on the
    /// first row, then the text rows, then its lower rule on the last row
    /// when `footer` has rows. Empty when the strip does not fit.
    pub(super) detail: Rect,
    /// The `ui.tab_bar_right` status rows.
    pub(super) footer: Rect,
    pub(super) menu: Rect,
}

/// What the optional rows of `plan_layout` ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LayoutWants {
    /// Pinned rows (Browser, News, coordinator) that want to show.
    pub(super) pinned: u16,
    /// Status footer lines (`status_footer_lines`).
    pub(super) status_lines: usize,
    /// The Active agents block's line cap (`active_cap`).
    pub(super) active_cap: u16,
    /// The detail strip's text rows (`detail_lines`), without its rules.
    pub(super) detail_lines: u16,
}

/// The rows the detail strip takes for `lines` text rows: its upper rule,
/// the text, and the lower rule while the footer shows.
fn detail_rows(lines: u16, footer: u16) -> u16 {
    if lines == 0 {
        0
    } else {
        lines + 1 + u16::from(footer > 0)
    }
}

/// The height budget of `content` (pure). The toolbar and the menu row take
/// one row each; the status footer one row per line from `SPACIOUS_HEIGHT`
/// rows, else one row for all of them; the Active agents block the rows
/// `active_rows(cap)` asks for; the detail strip its text rows and rules;
/// the pinned rows one each while the list keeps a row
/// (`pinned_rows_that_fit` picks which stay in `plan.pinned.height`). The
/// list takes the rest. While it has fewer than `MIN_LIST_ROWS` rows the
/// optional rows give way in turn: the detail strip, then the Active block's
/// lines (cap 6, 4, 2), then the whole block, then the footer (one row, then
/// none).
pub(super) fn plan_layout(
    content: Rect,
    wants: LayoutWants,
    active_rows: impl Fn(u16) -> u16,
) -> SidebarPlan {
    let height = content.height;
    let room = height.saturating_sub(TOOLBAR_ROWS + FOOTER_ROWS);
    let status_lines = wants.status_lines.min(MAX_STATUS_ROWS);
    let mut footer = if height < SPACIOUS_HEIGHT {
        status_lines.min(1)
    } else {
        status_lines
    } as u16;
    let mut detail = wants.detail_lines;
    let mut active = active_rows(wants.active_cap);
    // The pinned rows keep one list row (the floor that never hides the
    // last tab); `pinned_rows_that_fit` ranks them.
    let pinned = wants.pinned.min(room.saturating_sub(1));
    let list = |footer: u16, detail: u16, active: u16, pinned: u16| {
        room.saturating_sub(
            footer
                .saturating_add(detail_rows(detail, footer))
                .saturating_add(active)
                .saturating_add(pinned),
        )
    };
    let short =
        |footer, detail, active, pinned| list(footer, detail, active, pinned) < MIN_LIST_ROWS;
    if short(footer, detail, active, pinned) {
        detail = 0;
    }
    for cap in ACTIVE_CAP_STEPS {
        if !short(footer, detail, active, pinned) {
            break;
        }
        if cap < wants.active_cap {
            active = active.min(active_rows(cap));
        }
    }
    if short(footer, detail, active, pinned) {
        active = 0;
    }
    while footer > 0 && short(footer, detail, active, pinned) {
        footer = if footer > 1 { 1 } else { 0 };
    }
    let list_height = list(footer, detail, active, pinned);
    let detail_height = detail_rows(detail, footer);

    let toolbar = Rect::new(
        content.x,
        content.y,
        content.width,
        TOOLBAR_ROWS.min(height),
    );
    let active = Rect::new(content.x, toolbar.bottom(), content.width, active);
    let list = Rect::new(content.x, active.bottom(), content.width, list_height);
    let pinned = Rect::new(content.x, list.bottom(), content.width, pinned);
    let detail = Rect::new(content.x, pinned.bottom(), content.width, detail_height);
    let footer = Rect::new(content.x, detail.bottom(), content.width, footer);
    let menu = Rect::new(
        content.x,
        content.bottom().saturating_sub(FOOTER_ROWS),
        content.width,
        FOOTER_ROWS.min(height),
    );
    SidebarPlan {
        toolbar,
        active,
        list,
        pinned,
        detail,
        footer,
        menu,
    }
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

    fn kind(&self) -> PinnedKind {
        match self {
            Self::Browser(_) => PinnedKind::Browser,
            Self::News(_) => PinnedKind::News,
            Self::Coordinator(_) => PinnedKind::Coordinator,
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

/// A pinned row: ` ▎ <glyph> <label> … <status> `, the glyph at x=3 in its
/// state's color, the label at x=5, the status dim and right-aligned with a
/// one-cell margin. The focused News / coordinator row has the focused row
/// background and a bold label; a hovered row has the bar and the hover
/// band. On the Browser and coordinator rows the label wins over the status
/// in a narrow sidebar (the status is truncated first); on the News row the
/// status wins.
fn render_pinned_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &PinnedRow<'_>,
    hovered: bool,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let (glyph, glyph_color, label, status, focused, label_first) = match row {
        PinnedRow::Browser(row) => (
            row.state.glyph(),
            row.state.color(palette, row.active),
            super::browser::BROWSER_ROW_LABEL,
            row.status.as_str(),
            false,
            true,
        ),
        PinnedRow::News(row) => (
            row.state.glyph(),
            row.state.color(palette),
            super::news::NEWS_ROW_LABEL,
            row.status.as_str(),
            row.focused,
            false,
        ),
        PinnedRow::Coordinator(row) => (
            row.state.glyph(),
            row.state.color(palette),
            super::coordinator::COORDINATOR_ROW_LABEL,
            row.status.as_str(),
            row.focused,
            true,
        ),
    };
    if hovered {
        buffer.set_style(rect, Style::default().bg(palette.sidebar_hover_bg()));
        put_text(
            buffer,
            rect.x,
            rect.y,
            1,
            HOVER_BAR,
            Style::default().fg(palette.accent),
        );
    } else if focused {
        buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
    }
    let label_style = if focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.subtext0)
    };
    const GLYPH_X: u16 = 3;
    const LABEL_X: u16 = 5;
    put_text(
        buffer,
        rect.x.saturating_add(GLYPH_X),
        rect.y,
        rect.width.saturating_sub(GLYPH_X).min(display_width(glyph)),
        glyph,
        Style::default().fg(glyph_color),
    );
    // Cells for the status: all of it, or (label first) what the label and
    // the gaps around it leave.
    let status_width = display_width(status);
    let status_cells = if label_first {
        status_width.min(
            rect.width
                .saturating_sub(LABEL_X + display_width(label) + 2),
        )
    } else {
        status_width
    };
    let label_cells = rect.width.saturating_sub(LABEL_X + status_cells + 2);
    put_truncated(
        buffer,
        rect.x.saturating_add(LABEL_X),
        rect.y,
        label_cells,
        label,
        label_style,
    );
    put_truncated(
        buffer,
        rect.right().saturating_sub(1 + status_cells),
        rect.y,
        status_cells,
        status,
        Style::default().fg(palette.overlay1),
    );
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

/// The status footer in `footer`, dim, one row per line with the rows'
/// one-cell margins; SGR styles apply over the dim base and each row is
/// truncated from the right with `…`. With fewer rows than lines the last
/// row joins the rest, two spaces apart; each line's styles are parsed on
/// their own (they reset per line), so the colors do not shift.
fn render_tab_status_footer(
    buffer: &mut Buffer,
    footer: Rect,
    lines: &[String],
    palette: &Palette,
) {
    const JOIN: &str = "  ";
    let width = footer.width.saturating_sub(2);
    let base = Style::default().fg(palette.overlay1);
    let rows = usize::from(footer.height);
    for row in 0..rows.min(lines.len()) {
        let spans = if row + 1 == rows && lines.len() > rows {
            let mut joined = Vec::new();
            for (index, line) in lines[row..].iter().enumerate() {
                if index > 0 {
                    joined.push(Span::styled(JOIN, base));
                }
                joined.extend(styled_status_spans(line, base));
            }
            joined
        } else {
            styled_status_spans(&lines[row], base)
        };
        let spans = truncate_status_spans(spans, usize::from(width));
        put_status_spans(
            buffer,
            footer.x.saturating_add(1),
            footer.y.saturating_add(row as u16),
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
        let toggle_width = display_width(toggle);
        put_text(buffer, x, y, toggle_width, toggle, style);
        if config.mouse_capture {
            hits.group_toggle_all = Rect::new(x, y, toggle_width, 1);
        }
    }
    let new_width = display_width(NEW_GROUP_LABEL);
    let new_x = content.right().saturating_sub(new_width + 1);
    put_text(buffer, new_x, y, new_width, NEW_GROUP_LABEL, style);
    if config.mouse_capture {
        hits.group_new = Rect::new(new_x, y, new_width, 1);
    }
}

/// One group header's state for a frame.
#[derive(Debug, Clone, Copy)]
struct HeaderLook {
    folded: bool,
    /// Non-pinned member tabs.
    members: u16,
    dragged: bool,
    hovered: bool,
}

/// A group header: ` ▾ ◆ <name> … <count> <status> `. No band (only a
/// surface1 one while dragged); the hovered header has the bar. The name is
/// bold: `text` for the focused or hovered group, `overlay1` folded,
/// `subtext0` otherwise; a team shows `◆` in the icon slot and its purpose
/// (or its label in overlay0 before it has one). The name is the only part
/// that truncates.
fn render_group_header(
    buffer: &mut Buffer,
    rect: Rect,
    workspace: &crate::protocol::ClientShellWorkspace,
    look: HeaderLook,
    team: Option<&crate::api::schema::team::TeamInfo>,
    config: &ClientShellConfig,
) {
    const MARKER_X: u16 = 1;
    const MARK_X: u16 = 3;
    const NAME_X: u16 = 5;
    let palette = &config.palette;
    let (x, y, width) = (rect.x, rect.y, rect.width);
    if look.dragged {
        buffer.set_style(rect, Style::default().bg(palette.surface1));
    }
    if look.hovered {
        put_text(
            buffer,
            x,
            y,
            1,
            HOVER_BAR,
            Style::default().fg(palette.accent),
        );
    }
    let (marker, marker_color) = if look.folded {
        (GROUP_FOLDED_MARKER, palette.overlay1)
    } else {
        (GROUP_OPEN_MARKER, palette.subtext0)
    };
    put_text(
        buffer,
        x.saturating_add(MARKER_X),
        y,
        width.saturating_sub(MARKER_X).min(1),
        marker,
        Style::default().fg(marker_color),
    );
    let name_color = if workspace.focused || look.hovered {
        palette.text
    } else if look.folded {
        palette.overlay1
    } else {
        palette.subtext0
    };
    let (name, name_color) = match team {
        Some(team) => {
            let (name, placeholder) = super::teams::header_label(team, &workspace.label);
            let mark_color = if placeholder {
                palette.overlay0
            } else {
                palette.accent
            };
            put_text(
                buffer,
                x.saturating_add(MARK_X),
                y,
                width.saturating_sub(MARK_X).min(1),
                super::teams::TEAM_MARK,
                Style::default().fg(mark_color),
            );
            (
                name,
                if placeholder {
                    palette.overlay0
                } else {
                    name_color
                },
            )
        }
        None => (workspace.label.as_str(), name_color),
    };
    let mut count = StackStr::<8>::new();
    let _ = write!(count, "{}", look.members);
    let count_width = display_width(count.as_str());
    // `<count> <status> ` at the right edge: the count ends at width-4.
    put_text(
        buffer,
        x.saturating_add(width.saturating_sub(3 + count_width)),
        y,
        count_width.min(width),
        count.as_str(),
        Style::default().fg(palette.overlay0),
    );
    let status = status_icon(workspace.agent_status, config.status_indicators);
    put_text(
        buffer,
        x.saturating_add(width.saturating_sub(2)),
        y,
        width.min(1),
        status,
        Style::default().fg(status_color(workspace.agent_status, palette)),
    );
    put_truncated(
        buffer,
        x.saturating_add(NAME_X),
        y,
        width.saturating_sub(NAME_X + count_width + 4),
        name,
        Style::default().fg(name_color).add_modifier(Modifier::BOLD),
    );
}

/// A tab row's right-hand marks, at most four (browser `◎`, `★`, the
/// reminder, the harness glyph), on the stack.
#[derive(Debug, Clone, Copy)]
struct Marks<'c> {
    items: [(&'c str, Color); 4],
    len: usize,
}

impl Default for Marks<'_> {
    fn default() -> Self {
        Self {
            items: [("", Color::Reset); 4],
            len: 0,
        }
    }
}

impl<'c> Marks<'c> {
    fn push(&mut self, glyph: &'c str, color: Color) {
        if let Some(slot) = self.items.get_mut(self.len) {
            *slot = (glyph, color);
            self.len += 1;
        }
    }

    fn as_slice(&self) -> &[(&'c str, Color)] {
        &self.items[..self.len]
    }
}

/// A tab row's reminder marks, in order: browser `◎` (accent) when a pane
/// of the tab used the herdr browser recently, `★` when the tab is
/// important, the interval's marker (`remind_marker`) when it has a
/// scheduled reminder. The reminder marks are overlay0, or the fired
/// reminder's color while lit.
fn push_reminder_marks(
    marks: &mut Marks<'_>,
    tab: &crate::protocol::ClientShellTab,
    state: &ShellRenderState<'_>,
    palette: &Palette,
) {
    if state.browser_marked_tabs.contains(&tab.tab_id) {
        marks.push(super::browser::TAB_BROWSER_MARKER, palette.accent);
    }
    let remind_every = tab
        .remind_every
        .filter(|every| *every != crate::api::schema::TabRemindInterval::Unknown);
    if !tab.important && remind_every.is_none() {
        return;
    }
    let key = (state.active_endpoint_id.clone(), tab.tab_id.clone());
    if tab.important {
        let lit = state
            .idle_reminders
            .get(&key)
            .and_then(|reminder| reminder.lit);
        marks.push(
            TAB_IMPORTANT_MARKER,
            lit.map_or(palette.overlay0, |lit| lit.color(palette)),
        );
    }
    if let Some(every) = remind_every {
        let lit = state
            .scheduled_reminders
            .get(&key)
            .is_some_and(|reminder| reminder.lit);
        marks.push(
            remind_marker(every),
            if lit {
                super::idle_reminders::ClientReminderLit::Scheduled.color(palette)
            } else {
                palette.overlay0
            },
        );
    }
}

/// The selected row's harness glyph (`ui.tab_agent_glyphs` for the tab's
/// last agent, `shell` without one) in its brand color, breathing over
/// `background` while the tab works. `None` for an empty glyph.
struct HarnessMark<'c> {
    glyph: &'c str,
    color: Color,
    breathing: bool,
}

fn harness_mark<'c>(
    snapshot: &ClientShellSnapshot,
    tab: &crate::protocol::ClientShellTab,
    facts: &TabFacts,
    background: Color,
    state: &ShellRenderState<'_>,
    config: &'c ClientShellConfig,
) -> Option<HarnessMark<'c>> {
    let glyph_key = facts
        .primary_agent
        .and_then(|agent| snapshot.agents.get(agent as usize))
        .map_or("shell", |agent| agent.agent.as_deref().unwrap_or("other"));
    let glyph = crate::config::tab_agent_glyph(&config.tab_agent_glyphs, glyph_key);
    if glyph.is_empty() {
        return None;
    }
    let normal = crate::config::tab_agent_glyph_color(&config.tab_agent_glyph_colors, glyph_key)
        .unwrap_or(config.palette.overlay0);
    // Fork: a working tab's glyph breathes (`breathe.rs`).
    let breathing = tab.agent_status == crate::api::schema::AgentStatus::Working;
    let color = if breathing {
        super::breathe::glyph_color(
            normal,
            background,
            state.breathe_reset_rgb,
            state.breathe_phase,
        )
    } else {
        normal
    };
    Some(HarnessMark {
        glyph,
        color,
        breathing,
    })
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

/// One tab row's look for a frame.
struct TabRowLook<'a, 'c> {
    hovered: bool,
    /// The row band: the hover band, else the focused row's; `None` keeps
    /// the sidebar background.
    background: Option<Color>,
    /// The voice mark (`voice::voice_mark`), left of the label.
    voice: Option<(&'c str, Color)>,
    /// The right-hand marks, left to right.
    marks: &'a [(&'c str, Color)],
}

/// The cells the marks take at the right edge, the one-cell margin
/// included: each mark and, with `gap`, one blank after every mark but the
/// last.
fn marks_cells(marks: &[(&str, Color)], gap: u16) -> u16 {
    if marks.is_empty() {
        return 0;
    }
    let glyphs: u16 = marks.iter().map(|(glyph, _)| display_width(glyph)).sum();
    glyphs + gap * (marks.len() as u16 - 1) + 1
}

/// A tab row: ` ▎   <status> <voice> <label> … <marks> `. The status icon
/// sits at x=5 (`sidebar_model::tab_row_icon`: the subagent icon while
/// subagents run), the voice mark at x=7 with the label after it, else the
/// label at x=7. The marks are packed flush right with a one-cell margin,
/// one blank between them (none when that leaves the label fewer than
/// `MIN_LABEL_CELLS`); the label gives way to them and is the only part that
/// truncates. The label is bold in the focused tab (tag color or `text`);
/// elsewhere it takes its tag color, else `text` while the agent works,
/// waits or finished, `overlay0` suspended and `subtext0` otherwise.
fn render_tab_row(
    buffer: &mut Buffer,
    rect: Rect,
    tab: &crate::protocol::ClientShellTab,
    subagents: u32,
    look: TabRowLook<'_, '_>,
    config: &ClientShellConfig,
) {
    use crate::api::schema::AgentStatus;
    const ICON_X: u16 = 5;
    const LABEL_X: u16 = 7;
    let palette = &config.palette;
    let (x, y, width) = (rect.x, rect.y, rect.width);
    if let Some(background) = look.background {
        buffer.set_style(rect, Style::default().bg(background));
    }
    if look.hovered {
        put_text(
            buffer,
            x,
            y,
            1,
            HOVER_BAR,
            Style::default().fg(palette.accent),
        );
    }
    let icon =
        super::sidebar_model::tab_row_icon(tab.agent_status, subagents, config.status_indicators);
    put_text(
        buffer,
        x.saturating_add(ICON_X),
        y,
        width.saturating_sub(ICON_X).min(display_width(icon)),
        icon,
        Style::default().fg(status_color(tab.agent_status, palette)),
    );
    let mut label_x = LABEL_X;
    if let Some((glyph, color)) = look.voice {
        let glyph_width = display_width(glyph);
        put_text(
            buffer,
            x.saturating_add(LABEL_X),
            y,
            width.saturating_sub(LABEL_X).min(glyph_width),
            glyph,
            Style::default().fg(color),
        );
        label_x = LABEL_X + glyph_width + 1;
    }
    // A color tag only changes the label's foreground; background, the
    // status icon and the marks keep their own styling.
    let tag_fg = super::tab_color::tab_label_fg(tab.color, palette);
    let label_style = if tab.focused {
        Style::default()
            .fg(tag_fg.unwrap_or(palette.text))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(tag_fg.unwrap_or(match tab.agent_status {
            AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done => palette.text,
            AgentStatus::Suspended => palette.overlay0,
            AgentStatus::Idle | AgentStatus::Unknown => palette.subtext0,
        }))
    };
    let label_cells = |marks_width: u16| width.saturating_sub(label_x + marks_width + 1);
    let mut gap = 1;
    let mut marks_width = marks_cells(look.marks, gap);
    if !look.marks.is_empty() && label_cells(marks_width) < MIN_LABEL_CELLS {
        gap = 0;
        marks_width = marks_cells(look.marks, gap);
    }
    put_truncated(
        buffer,
        x.saturating_add(label_x),
        y,
        label_cells(marks_width),
        &tab.label,
        label_style,
    );
    let mut mark_x = x.saturating_add(width.saturating_sub(marks_width));
    for (glyph, color) in look.marks {
        let glyph_width = display_width(glyph);
        put_text(
            buffer,
            mark_x,
            y,
            glyph_width.min(rect.right().saturating_sub(mark_x)),
            glyph,
            Style::default().fg(*color),
        );
        mark_x = mark_x.saturating_add(glyph_width + gap);
    }
}

fn display_width(text: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(text).min(usize::from(u16::MAX)) as u16
}
