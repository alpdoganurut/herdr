//! Muted tabs on the client (fork): the tab ids each endpoint's last
//! `endpoint.tab-mutes.v1` push listed (server side:
//! src/server/headless/tab_mutes.rs, src/app/tab_mute.rs).
//!
//! A state is used only while its `boot_id` is that endpoint's snapshot's,
//! like pins, voice and agent times. Three readers:
//!
//! - the notification policy and the reminders (`tab_muted_on`): a muted
//!   tab's toasts, sounds, system / terminal notifications and reminder
//!   alerts are dropped on the client too (the server already drops the
//!   ones it raises). Agent cards are another path and still show.
//! - the tabs sidebar model (one set lookup per tab, only while some tab is
//!   muted): a muted tab shows `mute_mark` in the row's left gutter and a
//!   `muted` chip in the detail strip.
//! - the tab menu (`active_tab_muted`), once when it opens.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::server::headless::tab_mutes::TabMutesPayload;

/// The mark of a muted tab: Nerd Font `nf-md-bell_off`, one cell wide.
pub(crate) const MUTED_MARK: &str = "\u{F009B}";
/// The `ui.tab_agent_glyphs` key that overrides the mark.
pub(crate) const MUTED_GLYPH_KEY: &str = "muted";

/// One endpoint's muted tabs, as its last push listed them.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClientTabMutesState {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    /// Public tab ids.
    pub(crate) tab_ids: HashSet<String>,
}

impl ClientTabMutesState {
    pub(crate) fn from_payload(payload: TabMutesPayload) -> Self {
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            tab_ids: payload.tab_ids.into_iter().collect(),
        }
    }

    pub(crate) fn is_muted(&self, tab_id: &str) -> bool {
        self.tab_ids.contains(tab_id)
    }
}

/// The active endpoint's mutes, while they belong to the active snapshot's
/// server.
pub(crate) fn active_tab_mutes_of<'a>(
    mutes: &'a HashMap<ClientEndpointId, ClientTabMutesState>,
    endpoint_id: &ClientEndpointId,
    snapshot: Option<&ClientShellSnapshot>,
) -> Option<&'a ClientTabMutesState> {
    let snapshot = snapshot?;
    mutes
        .get(endpoint_id)
        .filter(|mutes| mutes.boot_id == snapshot.boot_id)
}

/// The muted tab mark and its color: `ui.tab_agent_glyphs.muted` overrides
/// the glyph (exact key, no `other` fallback), dim `overlay0`.
pub(crate) fn mute_mark(config: &ClientShellConfig) -> (&str, ratatui::style::Color) {
    let glyph = config
        .tab_agent_glyphs
        .get(MUTED_GLYPH_KEY)
        .map_or(MUTED_MARK, String::as_str);
    (glyph, config.palette.overlay0)
}

impl ClientShellState {
    /// An `endpoint.tab-mutes.v1` push. Returns whether to repaint. An
    /// older or equal revision of the same server is ignored; a new server
    /// replaces the state.
    pub(crate) fn receive_tab_mutes(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: TabMutesPayload,
    ) -> bool {
        if self.tab_mutes.get(endpoint_id).is_some_and(|current| {
            current.boot_id == payload.boot_id && current.revision >= payload.revision
        }) {
            return false;
        }
        self.tab_mutes.insert(
            endpoint_id.clone(),
            ClientTabMutesState::from_payload(payload),
        );
        self.sidebar_model.mark_dirty();
        true
    }

    /// Whether `tab_id` is muted on the active endpoint (the tab menu).
    pub(super) fn active_tab_muted(&self, tab_id: &str) -> bool {
        active_tab_mutes_of(
            &self.tab_mutes,
            &self.active_endpoint_id,
            self.snapshot.as_deref(),
        )
        .is_some_and(|mutes| mutes.is_muted(tab_id))
    }

    /// Whether `tab_id` is muted on `endpoint_id` (notifications and
    /// reminders carry their own endpoint). Free while that endpoint never
    /// pushed a mute.
    pub(super) fn tab_muted_on(&self, endpoint_id: &ClientEndpointId, tab_id: &str) -> bool {
        let Some(mutes) = self
            .tab_mutes
            .get(endpoint_id)
            .filter(|mutes| !mutes.tab_ids.is_empty())
        else {
            return false;
        };
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .is_some_and(|snapshot| snapshot.boot_id == mutes.boot_id && mutes.is_muted(tab_id))
    }

    /// Whether a notification for `event` is silenced by its tab's mute.
    /// Everything this client raises from a tab's events goes through here
    /// (toasts, their sounds, terminal / system notifications, reminders);
    /// agent cards do not.
    pub(super) fn notification_muted(
        &self,
        endpoint_id: &ClientEndpointId,
        event: &SemanticNotification,
    ) -> bool {
        event
            .tab_id
            .as_deref()
            .is_some_and(|tab_id| self.tab_muted_on(endpoint_id, tab_id))
    }
}
