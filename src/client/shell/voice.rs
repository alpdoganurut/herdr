//! Voice modes on the client (fork): the client-only copy of each
//! endpoint's panes in voice mode, as its last `endpoint.voice.v1` push
//! listed them (server side: src/server/headless/voice.rs).
//!
//! The `tabs` sidebar marks a tab whose agent is in voice mode, left of its
//! label: a red microphone (U+F130) while it is live, a dim crossed-out one
//! (U+F131) while it is muted, nothing when voice mode is off. Both are Nerd
//! Font symbols; `ui.tab_agent_glyphs.voice_live` / `voice_muted` override
//! them (an exact-key lookup, e.g. `voice_live = "●"` for fonts without the
//! symbols). The lookup map is built once per push, so the mark is an O(1)
//! lookup in the sidebar model's pass over the snapshot's agents. A state is
//! used only while its `boot_id` is the active snapshot's.

use std::collections::HashMap;

use super::*;
use crate::api::schema::AgentVoiceMode;
use crate::server::headless::voice::VoicePayload;

/// The mark of a tab whose agent listens (microphone live): Nerd Font
/// `nf-fa-microphone`, one cell wide.
pub(crate) const VOICE_LIVE_MARK: &str = "\u{F130}";
/// The mark of a tab whose agent's voice session is muted: Nerd Font
/// `nf-fa-microphone_slash`, one cell wide.
pub(crate) const VOICE_MUTED_MARK: &str = "\u{F131}";
/// `ui.tab_agent_glyphs` keys that override the two marks.
pub(crate) const VOICE_LIVE_GLYPH_KEY: &str = "voice_live";
pub(crate) const VOICE_MUTED_GLYPH_KEY: &str = "voice_muted";

/// One endpoint's panes in voice mode, as its last push listed them.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClientVoiceState {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    /// Public pane id → voice mode; rebuilt only when a push arrives.
    pub(crate) panes: HashMap<String, AgentVoiceMode>,
}

impl ClientVoiceState {
    pub(crate) fn from_payload(payload: VoicePayload) -> Self {
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            panes: payload
                .panes
                .into_iter()
                .map(|pane| (pane.pane_id, pane.voice))
                .collect(),
        }
    }

    /// A pane's voice mode; `None` while it is off.
    pub(crate) fn voice_of(&self, pane_id: &str) -> Option<AgentVoiceMode> {
        self.panes.get(pane_id).copied()
    }
}

/// Of two agents in one tab, the one whose voice mode leads the mark: a
/// live microphone outranks a muted one.
pub(crate) fn louder(
    current: Option<AgentVoiceMode>,
    next: AgentVoiceMode,
) -> Option<AgentVoiceMode> {
    match current {
        Some(AgentVoiceMode::Muted) | None => Some(next),
        live => live,
    }
}

/// The tab mark of a voice mode and its color (an unknown mode counts as
/// live: the voice session is on). The glyph borrows the config:
/// `ui.tab_agent_glyphs.voice_live` / `voice_muted` override it (exact key,
/// no `other` fallback).
pub(crate) fn voice_mark(
    voice: AgentVoiceMode,
    config: &ClientShellConfig,
) -> (&str, ratatui::style::Color) {
    let palette = &config.palette;
    let (key, builtin, color) = match voice {
        AgentVoiceMode::Muted => (VOICE_MUTED_GLYPH_KEY, VOICE_MUTED_MARK, palette.overlay0),
        AgentVoiceMode::Live | AgentVoiceMode::Unknown => {
            (VOICE_LIVE_GLYPH_KEY, VOICE_LIVE_MARK, palette.red)
        }
    };
    let glyph = config
        .tab_agent_glyphs
        .get(key)
        .map_or(builtin, String::as_str);
    (glyph, color)
}

/// The active endpoint's voice state, while it belongs to the active
/// snapshot's server.
pub(crate) fn active_voice_of<'a>(
    voice: &'a HashMap<ClientEndpointId, ClientVoiceState>,
    endpoint_id: &ClientEndpointId,
    snapshot: Option<&ClientShellSnapshot>,
) -> Option<&'a ClientVoiceState> {
    let snapshot = snapshot?;
    voice
        .get(endpoint_id)
        .filter(|voice| voice.boot_id == snapshot.boot_id)
}

impl ClientShellState {
    /// An `endpoint.voice.v1` push. Returns whether to repaint. An older or
    /// equal revision of the same server is ignored; a new server replaces
    /// the state.
    pub(crate) fn receive_voice(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: VoicePayload,
    ) -> bool {
        if self.voice.get(endpoint_id).is_some_and(|current| {
            current.boot_id == payload.boot_id && current.revision >= payload.revision
        }) {
            return false;
        }
        self.voice
            .insert(endpoint_id.clone(), ClientVoiceState::from_payload(payload));
        self.sidebar_model.mark_dirty();
        true
    }
}
