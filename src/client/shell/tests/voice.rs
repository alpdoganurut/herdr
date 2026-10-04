//! Voice modes on the client (fork): the `endpoint.voice.v1` state and the
//! tab row's voice mark left of the label (a red microphone live, a dim
//! crossed-out one muted, nothing off).

use super::teams::{tabs_config, team_snapshot};
use super::*;
use crate::client::shell::voice::{louder, VOICE_LIVE_MARK, VOICE_MUTED_MARK};
use crate::server::headless::voice::VoicePayload;

/// A push, built from its JSON shape (`boot_id`, `revision`, `panes`).
fn payload(boot_id: &str, revision: u64, panes: &[(&str, &str)]) -> VoicePayload {
    let panes: Vec<_> = panes
        .iter()
        .map(|(pane_id, voice)| serde_json::json!({ "pane_id": pane_id, "voice": voice }))
        .collect();
    serde_json::from_value(serde_json::json!({
        "boot_id": boot_id,
        "revision": revision,
        "panes": panes,
    }))
    .expect("voice payload")
}

fn state() -> ClientShellState {
    state_with(&tabs_config())
}

fn state_with(config: &crate::config::Config) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    state
}

/// The voice mark's column; the label follows two cells later.
const VOICE_X: u16 = 7;

/// A tab row: its text, the cell at the voice mark's column (symbol and
/// color) and the column its label starts at.
struct VoiceRow {
    text: String,
    mark: (String, ratatui::style::Color),
    label_x: Option<u16>,
}

fn voice_row(state: &mut ClientShellState, tab_id: &str, label: &str) -> VoiceRow {
    let frame = state.compose(106, 24).expect("composed frame");
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let (rect, _) = *state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .expect("tab row");
    let text: String = (rect.x..rect.right())
        .map(|x| buffer[(x, rect.y)].symbol().to_string())
        .collect();
    let cell = &buffer[(rect.x + VOICE_X, rect.y)];
    let first = label.chars().next().map(String::from).unwrap_or_default();
    let label_x = (rect.x..rect.right())
        .find(|x| buffer[(*x, rect.y)].symbol() == first)
        .map(|x| x - rect.x);
    VoiceRow {
        text,
        mark: (cell.symbol().to_string(), cell.fg),
        label_x,
    }
}

/// The row of `tab_id` without any voice push.
fn plain_row(tab_id: &str) -> String {
    voice_row(&mut state(), tab_id, "").text
}

#[test]
fn fork_smoke_a_live_tab_gets_a_red_dot_a_muted_one_a_dim_ring_and_an_off_one_nothing() {
    // Sidebar v2: the marks are microphones left of the label (the name
    // stays as FORK.md section 10 lists it).
    let mut state = state();
    assert!(state.receive_voice(
        &ClientEndpointId::Local,
        payload("boot-1", 1, &[("pane_2", "live"), ("pane_3", "muted")])
    ));
    let palette = state.config.palette.clone();

    let row = voice_row(&mut state, "tab_2", "fixer");
    assert_eq!(
        row.mark,
        (VOICE_LIVE_MARK.to_string(), palette.red),
        "live: {:?}",
        row.text
    );
    assert_eq!(row.label_x, Some(VOICE_X + 2), "{:?}", row.text);
    assert_eq!(row.text.matches(VOICE_LIVE_MARK).count(), 1);

    let row = voice_row(&mut state, "tab_3", "helper");
    assert_eq!(
        row.mark,
        (VOICE_MUTED_MARK.to_string(), palette.overlay0),
        "muted: {:?}",
        row.text
    );
    assert_eq!(row.label_x, Some(VOICE_X + 2), "{:?}", row.text);

    for (tab_id, label) in [("tab_1", "home"), ("tab_4", "shell")] {
        let row = voice_row(&mut state, tab_id, label);
        assert_eq!(row.text, plain_row(tab_id), "off: {tab_id}");
        assert_eq!(row.label_x, Some(VOICE_X), "off: {:?}", row.text);
    }

    // Voice mode ends: the next push lists nothing and the marks go.
    assert!(state.receive_voice(&ClientEndpointId::Local, payload("boot-1", 2, &[])));
    for tab_id in ["tab_2", "tab_3"] {
        let row = voice_row(&mut state, tab_id, "");
        assert_eq!(row.text, plain_row(tab_id), "off again: {tab_id}");
    }
}

#[test]
fn an_unknown_mode_counts_as_live_and_live_outranks_muted_in_one_tab() {
    let mut state = state();
    state.receive_voice(
        &ClientEndpointId::Local,
        payload("boot-1", 1, &[("pane_2", "speaking")]),
    );
    let palette = state.config.palette.clone();
    let row = voice_row(&mut state, "tab_2", "fixer");
    assert_eq!(row.mark, (VOICE_LIVE_MARK.to_string(), palette.red));

    use crate::api::schema::AgentVoiceMode::{Live, Muted};
    assert_eq!(louder(None, Muted), Some(Muted));
    assert_eq!(louder(Some(Muted), Live), Some(Live));
    assert_eq!(louder(Some(Live), Muted), Some(Live));
}

#[test]
fn voice_marks_take_the_tab_agent_glyphs_overrides() {
    let mut config = tabs_config();
    config
        .ui
        .tab_agent_glyphs
        .insert("voice_live".into(), "●".into());
    config
        .ui
        .tab_agent_glyphs
        .insert("voice_muted".into(), "◌".into());
    let mut state = state_with(&config);
    state.receive_voice(
        &ClientEndpointId::Local,
        payload("boot-1", 1, &[("pane_2", "live"), ("pane_3", "muted")]),
    );
    let palette = state.config.palette.clone();
    assert_eq!(
        voice_row(&mut state, "tab_2", "fixer").mark,
        ("●".to_string(), palette.red)
    );
    assert_eq!(
        voice_row(&mut state, "tab_3", "helper").mark,
        ("◌".to_string(), palette.overlay0)
    );
    // The microphones are one cell wide in a non-CJK terminal.
    for mark in [VOICE_LIVE_MARK, VOICE_MUTED_MARK] {
        assert_eq!(unicode_width::UnicodeWidthStr::width(mark), 1);
    }
}

#[test]
fn an_older_push_is_ignored_and_another_servers_list_is_never_drawn() {
    let mut state = state();
    assert!(state.receive_voice(
        &ClientEndpointId::Local,
        payload("boot-1", 2, &[("pane_2", "live")])
    ));
    assert!(!state.receive_voice(&ClientEndpointId::Local, payload("boot-1", 2, &[])));
    assert!(!state.receive_voice(&ClientEndpointId::Local, payload("boot-1", 1, &[])));
    let row = voice_row(&mut state, "tab_2", "fixer").text;
    assert_ne!(row, plain_row("tab_2"), "still live");

    // A list from another server boot is kept but never drawn.
    assert!(state.receive_voice(
        &ClientEndpointId::Local,
        payload("boot-0", 9, &[("pane_2", "live")])
    ));
    let row = voice_row(&mut state, "tab_2", "fixer").text;
    assert_eq!(row, plain_row("tab_2"));
}

#[test]
fn render_draws_the_plain_frame_without_voice_modes() {
    let mut plain = state();
    let before = plain.compose(106, 24).expect("frame");
    plain.receive_voice(&ClientEndpointId::Local, payload("boot-1", 1, &[]));
    let after = plain.compose(106, 24).expect("frame");
    assert_eq!(frame_rows(&before), frame_rows(&after));
}
