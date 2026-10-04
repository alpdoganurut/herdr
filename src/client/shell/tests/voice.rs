//! Voice modes on the client (fork): the `endpoint.voice.v1` state and the
//! tab row's recording mark (red `●` live, dim `◌` muted, nothing off).

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
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    state
}

/// The tab row's text and, when present, the color of the last `mark` in
/// it (the markers sit right of the label; a status glyph may lead).
fn tab_mark(
    state: &mut ClientShellState,
    tab_id: &str,
    mark: &str,
) -> (String, Option<ratatui::style::Color>) {
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
    let color = (rect.x..rect.right())
        .rev()
        .find(|x| buffer[(*x, rect.y)].symbol() == mark)
        .map(|x| buffer[(x, rect.y)].fg);
    (text, color)
}

/// The row of `tab_id` without any voice push.
fn plain_row(tab_id: &str) -> String {
    tab_mark(&mut state(), tab_id, VOICE_LIVE_MARK).0
}

fn count(row: &str, mark: &str) -> usize {
    row.matches(mark).count()
}

#[test]
fn fork_smoke_a_live_tab_gets_a_red_dot_a_muted_one_a_dim_ring_and_an_off_one_nothing() {
    let mut state = state();
    assert!(state.receive_voice(
        &ClientEndpointId::Local,
        payload("boot-1", 1, &[("pane_2", "live"), ("pane_3", "muted")])
    ));
    let palette = state.config.palette.clone();

    let (row, color) = tab_mark(&mut state, "tab_2", VOICE_LIVE_MARK);
    let plain = plain_row("tab_2");
    assert_eq!(
        count(&row, VOICE_LIVE_MARK),
        count(&plain, VOICE_LIVE_MARK) + 1,
        "{row:?}"
    );
    assert_eq!(
        count(&row, VOICE_MUTED_MARK),
        count(&plain, VOICE_MUTED_MARK),
        "{row:?}"
    );
    assert_eq!(color, Some(palette.red), "live: {row:?}");

    let (row, color) = tab_mark(&mut state, "tab_3", VOICE_MUTED_MARK);
    let plain = plain_row("tab_3");
    assert_eq!(
        count(&row, VOICE_MUTED_MARK),
        count(&plain, VOICE_MUTED_MARK) + 1,
        "{row:?}"
    );
    assert_eq!(
        count(&row, VOICE_LIVE_MARK),
        count(&plain, VOICE_LIVE_MARK),
        "{row:?}"
    );
    assert_eq!(color, Some(palette.overlay0), "muted: {row:?}");

    for tab_id in ["tab_1", "tab_4"] {
        let (row, _) = tab_mark(&mut state, tab_id, VOICE_LIVE_MARK);
        assert_eq!(row, plain_row(tab_id), "off: {tab_id}");
    }

    // Voice mode ends: the next push lists nothing and the marks go.
    assert!(state.receive_voice(&ClientEndpointId::Local, payload("boot-1", 2, &[])));
    for tab_id in ["tab_2", "tab_3"] {
        let (row, _) = tab_mark(&mut state, tab_id, VOICE_LIVE_MARK);
        assert_eq!(row, plain_row(tab_id), "off again: {tab_id}");
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
    let (row, color) = tab_mark(&mut state, "tab_2", VOICE_LIVE_MARK);
    assert_eq!(color, Some(palette.red), "{row:?}");

    use crate::api::schema::AgentVoiceMode::{Live, Muted};
    assert_eq!(louder(None, Muted), Some(Muted));
    assert_eq!(louder(Some(Muted), Live), Some(Live));
    assert_eq!(louder(Some(Live), Muted), Some(Live));
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
    let (row, _) = tab_mark(&mut state, "tab_2", VOICE_LIVE_MARK);
    assert_ne!(row, plain_row("tab_2"), "still live");

    // A list from another server boot is kept but never drawn.
    assert!(state.receive_voice(
        &ClientEndpointId::Local,
        payload("boot-0", 9, &[("pane_2", "live")])
    ));
    let (row, _) = tab_mark(&mut state, "tab_2", VOICE_LIVE_MARK);
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
