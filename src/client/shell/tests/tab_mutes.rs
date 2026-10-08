//! Fork: muted tabs on the client: the `endpoint.tab-mutes.v1` state, the
//! tab menu's Mute / Unmute item, the bell-off mark in the row's gutter and
//! the detail chip, and what a mute silences (toasts, their sounds,
//! reminders) and what it does not (agent cards).
//!
//! The fixture is sidebar_active.rs's (`t_block` blocked, `t_done`
//! finished, `t_focus` focused, all on the `boot-1` server).

use std::time::{Duration, Instant};

use super::sidebar_active::{
    active_snapshot, active_state, active_state_with, detail_text, endpoint_methods, mouse,
    tabs_config, COLS, ROWS,
};
use super::*;
use crate::api::schema::{AgentNoticeInfo, AgentNoticeKind, Method};
use crate::client::shell::tab_mutes::MUTED_MARK;
use crate::server::headless::agent_notices::AgentNoticesPayload;
use crate::server::headless::tab_mutes::TabMutesPayload;
use crossterm::event::{MouseButton, MouseEventKind};

/// The muted mark's column in a tab row (the hover bar is at 0, the status
/// icon at 5).
const MUTED_X: u16 = 3;

fn mutes_payload(boot_id: &str, revision: u64, tab_ids: &[&str]) -> TabMutesPayload {
    TabMutesPayload {
        boot_id: boot_id.into(),
        revision,
        tab_ids: tab_ids.iter().map(|id| (*id).to_string()).collect(),
    }
}

/// Each tab row's id and the symbol and color at its muted-mark column.
fn gutter_cells(state: &mut ClientShellState) -> Vec<(String, String, ratatui::style::Color)> {
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, id)| {
            let cell = &buffer[(rect.x + MUTED_X, rect.y)];
            (id.clone(), cell.symbol().to_string(), cell.fg)
        })
        .collect()
}

fn open_menu_on(state: &mut ClientShellState, tab_id: &str) -> Vec<ClientContextMenuItem> {
    state.overlay = None;
    state.compose(COLS, ROWS).expect("composed frame");
    let (row, _) = *state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .expect("tab row");
    mouse(
        state,
        MouseEventKind::Down(MouseButton::Right),
        row.x + 8,
        row.y,
    );
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("tab context menu"),
    }
}

#[test]
fn a_muted_tab_shows_the_bell_off_mark_at_column_3_and_no_other_row_does() {
    let mut state = active_state();
    let before = gutter_cells(&mut state);
    assert!(
        before.iter().all(|(_, symbol, _)| symbol == " "),
        "{before:?}"
    );
    assert!(state.receive_tab_mutes(
        &ClientEndpointId::Local,
        mutes_payload("boot-1", 1, &["t_work"])
    ));
    let overlay0 = state.config.palette.overlay0;
    let cells = gutter_cells(&mut state);
    assert!(cells.iter().any(|(id, _, _)| id == "t_work"));
    for (id, symbol, fg) in cells {
        if id == "t_work" {
            assert_eq!((symbol.as_str(), fg), (MUTED_MARK, overlay0), "{id}");
        } else {
            assert_eq!(symbol, " ", "{id} is not muted");
        }
    }
    // One cell wide in a non-CJK terminal, so the status icon keeps x=5.
    assert_eq!(unicode_width::UnicodeWidthStr::width(MUTED_MARK), 1);

    // Unmuted again: the mark goes.
    assert!(state.receive_tab_mutes(&ClientEndpointId::Local, mutes_payload("boot-1", 2, &[])));
    assert!(gutter_cells(&mut state)
        .iter()
        .all(|(_, symbol, _)| symbol == " "));
}

#[test]
fn the_muted_mark_takes_the_tab_agent_glyphs_override() {
    let mut config = tabs_config();
    config
        .ui
        .tab_agent_glyphs
        .insert("muted".into(), "M".into());
    let mut state = active_state_with(ClientShellConfig::from_config(&config));
    state.receive_tab_mutes(
        &ClientEndpointId::Local,
        mutes_payload("boot-1", 1, &["t_done"]),
    );
    let cells = gutter_cells(&mut state);
    let (_, symbol, _) = cells
        .iter()
        .find(|(id, _, _)| id == "t_done")
        .expect("t_done row");
    assert_eq!(symbol, "M");
}

#[test]
fn a_muted_tab_says_so_in_the_detail_strip() {
    let mut state = active_state();
    state.receive_tab_mutes(
        &ClientEndpointId::Local,
        mutes_payload("boot-1", 1, &["t_work"]),
    );
    state.compose(COLS, ROWS).expect("composed frame");
    let row_of = |state: &ClientShellState, tab_id: &str| {
        state
            .hits
            .sidebar_tabs
            .iter()
            .find(|(_, id)| id == tab_id)
            .map(|(rect, _)| *rect)
            .expect("tab row")
    };
    let row = row_of(&state, "t_work");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text = detail_text(&state, &frame).join(" ");
    assert!(text.contains("muted"), "{text:?}");
    let row = row_of(&state, "t_done");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text = detail_text(&state, &frame).join(" ");
    assert!(!text.contains("muted"), "{text:?}");
}

#[test]
fn another_servers_mutes_are_never_used_and_older_pushes_are_ignored() {
    let mut state = active_state();
    assert!(state.receive_tab_mutes(
        &ClientEndpointId::Local,
        mutes_payload("boot-other", 5, &["t_work"])
    ));
    assert!(gutter_cells(&mut state)
        .iter()
        .all(|(_, symbol, _)| symbol == " "));
    assert!(!state.tab_muted_on(&ClientEndpointId::Local, "t_work"));
    assert!(!state.active_tab_muted("t_work"));
    assert!(state.receive_tab_mutes(
        &ClientEndpointId::Local,
        mutes_payload("boot-1", 1, &["t_work"])
    ));
    assert!(!state.receive_tab_mutes(&ClientEndpointId::Local, mutes_payload("boot-1", 1, &[])));
    assert!(state.tab_muted_on(&ClientEndpointId::Local, "t_work"));
    assert!(state.active_tab_muted("t_work"));
}

#[test]
fn mute_menu_item_toggles_through_tab_set_muted_and_hides_without_server_support() {
    let mut state = active_state();
    let items = open_menu_on(&mut state, "t_work");
    let index = items
        .iter()
        .position(|item| item.label == "Mute notifications")
        .expect("a Mute notifications item");
    let pin = items
        .iter()
        .position(|item| item.action == ClientContextMenuAction::Pin)
        .expect("Pin");
    assert_eq!(index, pin + 1, "right after Pin");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabSetMuted(params)] if params.tab_id == "t_work" && params.muted
    ));
    assert!(
        !endpoint_methods(&outcome)
            .iter()
            .any(|method| matches!(method, Method::TabFocus(_))),
        "muting focuses nothing"
    );

    // Once the push lists it, the item unmutes.
    assert!(state.receive_tab_mutes(
        &ClientEndpointId::Local,
        mutes_payload("boot-1", 1, &["t_work"])
    ));
    let items = open_menu_on(&mut state, "t_work");
    let index = items
        .iter()
        .position(|item| item.label == "Unmute notifications")
        .expect("an Unmute notifications item");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabSetMuted(params)] if params.tab_id == "t_work" && !params.muted
    ));

    // A server without `tab.set_muted`: no item.
    state.set_endpoint_methods(Some(vec!["tab.focus".into(), "tab.set_pinned".into()]));
    let items = open_menu_on(&mut state, "t_work");
    assert!(
        !items
            .iter()
            .any(|item| item.action == ClientContextMenuAction::Mute),
        "{:?}",
        items.iter().map(|item| item.label).collect::<Vec<_>>()
    );
}

fn agent_event(
    kind: SemanticNotificationKind,
    tab_id: &str,
    sound: SemanticNotificationSound,
) -> SemanticNotification {
    let ws = if tab_id == "t_block" { "ws_a" } else { "ws_b" };
    SemanticNotification {
        kind,
        title: format!("claude {tab_id}"),
        body: None,
        sound: Some(sound),
        agent: Some("claude".into()),
        workspace_id: Some(ws.into()),
        tab_id: Some(tab_id.into()),
        pane_id: Some(tab_id.replacen("t_", "p_", 1)),
        position: None,
    }
}

fn herdr_toasts(state: &mut ClientShellState) {
    state.config.toast_delivery = crate::config::ToastDelivery::Herdr;
    state.config.toast_delay_seconds = 0;
    state.config.toast_sticky = true;
    state.config.sound_enabled = true;
}

fn card(id: &str) -> AgentNoticeInfo {
    AgentNoticeInfo {
        id: id.into(),
        kind: AgentNoticeKind::Question,
        title: format!("title {id}"),
        body: None,
        agent: Some("claude".into()),
        name: "blocker".into(),
        pane_id: "p_block".into(),
        tab_id: Some("t_block".into()),
        tab_label: Some("needs input".into()),
        workspace_id: Some("ws_a".into()),
        workspace_label: Some("Alpha".into()),
        unix: 1_800_000_000,
        team: None,
        role: None,
    }
}

#[test]
fn a_muted_tabs_toasts_are_dropped_while_its_agent_card_still_shows_and_rings() {
    let mut state = active_state();
    herdr_toasts(&mut state);
    let local = ClientEndpointId::Local;
    let now = Instant::now();

    // Unmuted: NeedsAttention and Finished show and ring.
    let (effects, _) = state.receive_notification(
        &local,
        agent_event(
            SemanticNotificationKind::NeedsAttention,
            "t_block",
            SemanticNotificationSound::Request,
        ),
        now,
    );
    assert!(!effects.is_empty(), "an unmuted tab rings");
    let (effects, _) = state.receive_notification(
        &local,
        agent_event(
            SemanticNotificationKind::Finished,
            "t_done",
            SemanticNotificationSound::Done,
        ),
        now,
    );
    assert!(!effects.is_empty(), "an unmuted tab rings");
    assert_eq!(state.visible_notifications.len(), 2);
    state.visible_notifications.clear();

    // Muted: nothing shows, nothing rings, nothing waits.
    assert!(state.receive_tab_mutes(&local, mutes_payload("boot-1", 1, &["t_block", "t_done"])));
    for (kind, tab_id, sound) in [
        (
            SemanticNotificationKind::NeedsAttention,
            "t_block",
            SemanticNotificationSound::Request,
        ),
        (
            SemanticNotificationKind::Finished,
            "t_done",
            SemanticNotificationSound::Done,
        ),
    ] {
        let (effects, _) =
            state.receive_notification(&local, agent_event(kind, tab_id, sound), now);
        assert!(effects.is_empty(), "{tab_id}: {} effects", effects.len());
    }
    assert!(state.visible_notifications.is_empty());
    assert!(state.queued_notifications.is_empty());
    assert!(state.pending_notifications.is_empty());

    // An agent card from the muted tab still shows and rings.
    state.receive_agent_notices(
        &local,
        AgentNoticesPayload {
            boot_id: "boot-1".into(),
            revision: 1,
            notices: Vec::new(),
            initial: true,
        },
    );
    let (effects, repaint) = state.receive_agent_notices(
        &local,
        AgentNoticesPayload {
            boot_id: "boot-1".into(),
            revision: 2,
            notices: vec![card("n1")],
            initial: false,
        },
    );
    assert!(repaint);
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            ClientShellNotificationEffect::Sound {
                sound: crate::sound::Sound::Request,
                ..
            }
        )),
        "the card rings"
    );
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text: String = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(text.contains("title n1"), "the card shows");
}

#[test]
fn a_muted_tab_is_never_reminded() {
    let mut snapshot = active_snapshot();
    for tab in &mut snapshot.tabs {
        match tab.tab_id.as_str() {
            "t_block" => {
                tab.important = true;
                tab.remind_every = Some(crate::api::schema::TabRemindInterval::M5);
            }
            "t_done" => tab.important = true,
            _ => {}
        }
    }
    let mut state = active_state();
    herdr_toasts(&mut state);
    state.config.idle_reminder_minutes = 5;
    state.set_snapshot(Box::new(snapshot));
    let local = ClientEndpointId::Local;
    assert!(state.receive_tab_mutes(&local, mutes_payload("boot-1", 1, &["t_block"])));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    let (effects, _) = state.tick_notifications(t0 + Duration::from_secs(5 * 60 + 1));
    let reminded: Vec<_> = state
        .visible_notifications
        .iter()
        .chain(state.queued_notifications.iter())
        .map(|card| card.event.tab_id.clone().unwrap_or_default())
        .collect();
    assert_eq!(reminded, ["t_done"], "only the unmuted tab is reminded");
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, ClientShellNotificationEffect::Sound { .. }))
            .count(),
        1,
        "one reminder sound"
    );
    assert!(state.pending_notifications.is_empty());
}
