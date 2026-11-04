//! Quiet messages and do-not-disturb on the client (fork): what each
//! endpoint's last `endpoint.quiet.v1` push listed (server side:
//! src/server/headless/quiet.rs, src/app/quiet_messages.rs,
//! src/app/tab_dnd.rs).
//!
//! A state is used only while its `boot_id` is that endpoint's snapshot's,
//! like pins and mutes. Readers:
//!
//! - the tabs sidebar model (one set and one map lookup per tab, only while
//!   the push lists something): a tab in do-not-disturb shows `dnd_mark` in
//!   the row's left gutter (the muted mark's cell, which it takes over); a
//!   tab whose agents have quiet messages waiting shows `✉N` first in its
//!   right-hand marks (and in the session header).
//! - the tab menu (`active_tab_dnd`), once when it opens.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::server::headless::quiet::QuietPayload;

/// The mark of a tab in do-not-disturb: Nerd Font `nf-md-weather_night`
/// (a crescent moon), one cell wide.
pub(crate) const DND_MARK: &str = "\u{F0594}";
/// The `ui.tab_agent_glyphs` key that overrides the mark.
pub(crate) const DND_GLYPH_KEY: &str = "dnd";
/// The pending marks, `✉1` to `✉9`, then `✉9+`.
const PENDING_MARKS: [&str; 10] = [
    "\u{2709}1",
    "\u{2709}2",
    "\u{2709}3",
    "\u{2709}4",
    "\u{2709}5",
    "\u{2709}6",
    "\u{2709}7",
    "\u{2709}8",
    "\u{2709}9",
    "\u{2709}9+",
];

/// One endpoint's quiet state, as its last push listed it.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClientQuietState {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    /// Public tab ids in do-not-disturb.
    pub(crate) dnd_tab_ids: HashSet<String>,
    /// Quiet messages waiting, summed per public tab id.
    pub(crate) tab_counts: HashMap<String, u32>,
}

impl ClientQuietState {
    pub(crate) fn from_payload(payload: QuietPayload) -> Self {
        let mut tab_counts: HashMap<String, u32> = HashMap::new();
        for pane in payload.pending {
            if pane.count > 0 && !pane.tab_id.is_empty() {
                *tab_counts.entry(pane.tab_id).or_default() += pane.count;
            }
        }
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            dnd_tab_ids: payload.dnd_tab_ids.into_iter().collect(),
            tab_counts,
        }
    }

    pub(crate) fn is_dnd(&self, tab_id: &str) -> bool {
        self.dnd_tab_ids.contains(tab_id)
    }

    /// The quiet messages waiting for the tab's agents.
    pub(crate) fn pending(&self, tab_id: &str) -> u32 {
        self.tab_counts.get(tab_id).copied().unwrap_or(0)
    }

    /// Whether the state lists anything (the model skips lookups otherwise).
    pub(crate) fn is_empty(&self) -> bool {
        self.dnd_tab_ids.is_empty() && self.tab_counts.is_empty()
    }
}

/// The active endpoint's quiet state, while it belongs to the active
/// snapshot's server.
pub(crate) fn active_quiet_of<'a>(
    quiet: &'a HashMap<ClientEndpointId, ClientQuietState>,
    endpoint_id: &ClientEndpointId,
    snapshot: Option<&ClientShellSnapshot>,
) -> Option<&'a ClientQuietState> {
    let snapshot = snapshot?;
    quiet
        .get(endpoint_id)
        .filter(|quiet| quiet.boot_id == snapshot.boot_id)
}

/// The do-not-disturb mark and its color: `ui.tab_agent_glyphs.dnd`
/// overrides the glyph (exact key, no `other` fallback), `accent`.
pub(crate) fn dnd_mark(config: &ClientShellConfig) -> (&str, ratatui::style::Color) {
    let glyph = config
        .tab_agent_glyphs
        .get(DND_GLYPH_KEY)
        .map_or(DND_MARK, String::as_str);
    (glyph, config.palette.accent)
}

/// The pending mark (`✉N`, `✉9+` from ten) and its color, `overlay1`;
/// `None` for zero.
pub(crate) fn pending_mark(
    count: u32,
    palette: &Palette,
) -> Option<(&'static str, ratatui::style::Color)> {
    let index = usize::try_from(count).ok()?.checked_sub(1)?;
    let glyph = PENDING_MARKS.get(index).or(PENDING_MARKS.last()).copied()?;
    Some((glyph, palette.overlay1))
}

impl ClientShellState {
    /// An `endpoint.quiet.v1` push. Returns whether to repaint. An older or
    /// equal revision of the same server is ignored; a new server replaces
    /// the state.
    pub(crate) fn receive_quiet(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: QuietPayload,
    ) -> bool {
        if self.quiet.get(endpoint_id).is_some_and(|current| {
            current.boot_id == payload.boot_id && current.revision >= payload.revision
        }) {
            return false;
        }
        self.quiet
            .insert(endpoint_id.clone(), ClientQuietState::from_payload(payload));
        self.sidebar_model.mark_dirty();
        true
    }

    /// Whether `tab_id` is in do-not-disturb on the active endpoint (the
    /// tab menu).
    pub(super) fn active_tab_dnd(&self, tab_id: &str) -> bool {
        active_quiet_of(
            &self.quiet,
            &self.active_endpoint_id,
            self.snapshot.as_deref(),
        )
        .is_some_and(|quiet| quiet.is_dnd(tab_id))
    }
}
