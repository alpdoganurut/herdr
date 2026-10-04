//! The tabs sidebar's Active agents block (fork, sidebar v2): blocked,
//! voice, working and finished agents above the list, with their time in
//! state. Hidden entirely while nothing is active or when
//! `ui.sidebar_active_agents = false`.
//!
//! S0b stub: the block takes no rows and draws nothing yet.

// Sidebar v2 S0b: the layout plan calls `active_cap` / `active_block_rows`;
// the renderer is wired once the block draws.
#![allow(dead_code)]

use ratatui::{buffer::Buffer, layout::Rect};

use super::render::ShellRenderState;
use super::sidebar_model::{ActiveView, SidebarModel};
use super::*;

/// The most entry lines the block shows (folded past it into `+N more`) at
/// a sidebar content height.
pub(super) fn active_cap(content_height: u16) -> u16 {
    let _ = content_height;
    8
}

/// The rows the block takes (header, entry lines, `+N more`, its rule);
/// 0 while disabled or empty.
pub(super) fn active_block_rows(model: &SidebarModel, view: ActiveView, cap: u16) -> u16 {
    let _ = (model, view, cap);
    0
}

/// Draws the block into `rect` (already painted with the chrome background)
/// and registers its hits.
#[allow(clippy::too_many_arguments)] // the block's inputs; a struct would only rename them
pub(super) fn render_active_block(
    buffer: &mut Buffer,
    rect: Rect,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    view: ActiveView,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let _ = (buffer, rect, snapshot, model, view, config, state, hits);
}
