//! The tabs sidebar's detail strip (fork, sidebar v2): facts about the
//! selected row (`sidebar_model::resolve_selected`) between the pinned rows
//! and the status footer.
//!
//! S0b stub: the strip takes no rows and draws nothing yet.

// Sidebar v2 S0b: the layout plan calls `detail_lines`; the renderer is
// wired once the strip draws.
#![allow(dead_code)]

use ratatui::{buffer::Buffer, layout::Rect};

use super::render::ShellRenderState;
use super::sidebar_model::SidebarModel;
use super::*;

/// The strip's text rows at a sidebar content height (0: no strip).
pub(super) fn detail_lines(content_height: u16) -> u16 {
    let _ = content_height;
    0
}

/// Draws the strip into `rect` (already painted with the chrome background)
/// and registers its hit.
pub(super) fn render_detail_strip(
    buffer: &mut Buffer,
    rect: Rect,
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let _ = (buffer, rect, snapshot, model, config, state, hits);
}
