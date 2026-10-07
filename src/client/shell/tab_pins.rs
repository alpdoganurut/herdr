//! Pinned tabs on the client (fork, sidebar v3): the tab ids the endpoint's
//! last `endpoint.tab-pins.v1` push listed (server side:
//! src/server/headless/tab_pins.rs, src/app/tab_pin.rs).
//!
//! A state is used only while its `boot_id` is the active snapshot's
//! (`active_tab_pins_of`), like voice and agent times. The tabs sidebar
//! model reads it on rebuild (one set lookup per tab, only while some tab
//! is pinned); the tab menu reads it once when it opens.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::server::headless::tab_pins::TabPinsPayload;

/// One endpoint's pinned tabs, as its last push listed them.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClientTabPinsState {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    /// Public tab ids.
    pub(crate) tab_ids: HashSet<String>,
}

impl ClientTabPinsState {
    pub(crate) fn from_payload(payload: TabPinsPayload) -> Self {
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            tab_ids: payload.tab_ids.into_iter().collect(),
        }
    }

    pub(crate) fn is_pinned(&self, tab_id: &str) -> bool {
        self.tab_ids.contains(tab_id)
    }
}

/// The active endpoint's pins, while they belong to the active snapshot's
/// server.
pub(crate) fn active_tab_pins_of<'a>(
    pins: &'a HashMap<ClientEndpointId, ClientTabPinsState>,
    endpoint_id: &ClientEndpointId,
    snapshot: Option<&ClientShellSnapshot>,
) -> Option<&'a ClientTabPinsState> {
    let snapshot = snapshot?;
    pins.get(endpoint_id)
        .filter(|pins| pins.boot_id == snapshot.boot_id)
}

impl ClientShellState {
    /// An `endpoint.tab-pins.v1` push. Returns whether to repaint. An older
    /// or equal revision of the same server is ignored; a new server
    /// replaces the state.
    pub(crate) fn receive_tab_pins(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: TabPinsPayload,
    ) -> bool {
        if self.tab_pins.get(endpoint_id).is_some_and(|current| {
            current.boot_id == payload.boot_id && current.revision >= payload.revision
        }) {
            return false;
        }
        self.tab_pins.insert(
            endpoint_id.clone(),
            ClientTabPinsState::from_payload(payload),
        );
        self.sidebar_model.mark_dirty();
        true
    }

    /// Whether `tab_id` is pinned on the active endpoint (the tab menu).
    pub(super) fn active_tab_pinned(&self, tab_id: &str) -> bool {
        active_tab_pins_of(
            &self.tab_pins,
            &self.active_endpoint_id,
            self.snapshot.as_deref(),
        )
        .is_some_and(|pins| pins.is_pinned(tab_id))
    }
}
