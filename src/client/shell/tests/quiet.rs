//! Fork: quiet messages and do-not-disturb on the client: the
//! `endpoint.quiet.v1` state, the moon mark in the row's gutter (it takes
//! the muted mark's cell), the `✉N` pending mark, the detail chips and the
//! tab menu's Do not disturb item.
//!
//! The fixture is sidebar_active.rs's (`t_block` blocked, `t_done`
//! finished, `t_focus` focused, all on the `boot-1` server).

use super::sidebar_active::{
    active_state, active_state_with, detail_text, endpoint_methods, mouse, tabs_config, COLS, ROWS,
};
use super::*;
use crate::api::schema::Method;
use crate::client::shell::quiet::DND_MARK;
use crate::client::shell::tab_mutes::MUTED_MARK;
use crate::server::headless::quiet::{QuietPayload, QuietPendingPane};
use crate::server::headless::tab_mutes::TabMutesPayload;
use crossterm::event::{MouseButton, MouseEventKind};

/// The gutter column the muted and do-not-disturb marks share.
const MARK_X: u16 = 3;

fn quiet_payload(revision: u64, dnd: &[&str], pending: &[(&str, u32)]) -> QuietPayload {
    QuietPayload {
        boot_id: "boot-1".into(),
        revision,
        dnd_tab_ids: dnd.iter().map(|id| (*id).to_string()).collect(),
        pending: pending
            .iter()
            .map(|(tab_id, count)| QuietPendingPane {
                pane_id: tab_id.replacen("t_", "p_", 1),
                tab_id: (*tab_id).to_string(),
                count: *count,
            })
            .collect(),
    }
}

/// Each tab row's id, its gutter mark and the row's text.
fn rows(state: &mut ClientShellState) -> Vec<(String, String, ratatui::style::Color, String)> {
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, id)| {
            let cell = &buffer[(rect.x + MARK_X, rect.y)];
            let text: String = (rect.x..rect.right())
                .map(|x| buffer[(x, rect.y)].symbol().to_string())
                .collect();
            (id.clone(), cell.symbol().to_string(), cell.fg, text)
        })
        .collect()
}

fn row_of(state: &ClientShellState, tab_id: &str) -> ratatui::layout::Rect {
    state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .map(|(rect, _)| *rect)
        .expect("tab row")
}

#[test]
fn a_dnd_tab_shows_the_moon_at_column_3_and_takes_over_the_muted_cell() {
    let mut state = active_state();
    assert!(state.receive_quiet(&ClientEndpointId::Local, quiet_payload(1, &["t_work"], &[])));
    let accent = state.config.palette.accent;
    for (id, symbol, fg, _) in rows(&mut state) {
        if id == "t_work" {
            assert_eq!((symbol.as_str(), fg), (DND_MARK, accent), "{id}");
        } else {
            assert_eq!(symbol, " ", "{id} is not in do not disturb");
        }
    }
    assert_eq!(unicode_width::UnicodeWidthStr::width(DND_MARK), 1);

    // Muted as well: the moon wins the shared cell; muted alone shows the bell.
    state.receive_tab_mutes(
        &ClientEndpointId::Local,
        TabMutesPayload {
            boot_id: "boot-1".into(),
            revision: 1,
            tab_ids: vec!["t_work".into(), "t_done".into()],
        },
    );
    let cells = rows(&mut state);
    let mark = |id: &str| {
        cells
            .iter()
            .find(|(row, _, _, _)| row == id)
            .map(|(_, symbol, _, _)| symbol.clone())
            .unwrap()
    };
    assert_eq!(mark("t_work"), DND_MARK);
    assert_eq!(mark("t_done"), MUTED_MARK);

    // Off again: the moon goes.
    assert!(state.receive_quiet(&ClientEndpointId::Local, quiet_payload(2, &[], &[])));
    let cells = rows(&mut state);
    assert!(cells
        .iter()
        .any(|(id, symbol, _, _)| id == "t_work" && symbol == MUTED_MARK));
}

#[test]
fn the_dnd_mark_takes_the_tab_agent_glyphs_override() {
    let mut config = tabs_config();
    config.ui.tab_agent_glyphs.insert("dnd".into(), "D".into());
    let mut state = active_state_with(ClientShellConfig::from_config(&config));
    state.receive_quiet(&ClientEndpointId::Local, quiet_payload(1, &["t_done"], &[]));
    let cells = rows(&mut state);
    let (_, symbol, _, _) = cells
        .iter()
        .find(|(id, _, _, _)| id == "t_done")
        .expect("t_done row");
    assert_eq!(symbol, "D");
}

#[test]
fn pending_quiet_messages_show_as_a_count_in_the_row_and_the_detail_strip() {
    let mut state = active_state();
    state.receive_quiet(
        &ClientEndpointId::Local,
        quiet_payload(
            1,
            &["t_work"],
            &[("t_work", 2), ("t_work", 1), ("t_done", 12)],
        ),
    );
    let cells = rows(&mut state);
    let text = |id: &str| {
        cells
            .iter()
            .find(|(row, _, _, _)| row == id)
            .map(|(_, _, _, text)| text.clone())
            .unwrap()
    };
    assert!(
        text("t_work").contains("\u{2709}3"),
        "summed per tab: {:?}",
        text("t_work")
    );
    assert!(
        text("t_done").contains("\u{2709}9+"),
        "{:?}",
        text("t_done")
    );
    assert!(
        !text("t_block").contains('\u{2709}'),
        "{:?}",
        text("t_block")
    );

    let row = row_of(&state, "t_work");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let detail = detail_text(&state, &frame).join(" ");
    assert!(detail.contains("do not disturb"), "{detail:?}");
    assert!(detail.contains("3 quiet"), "{detail:?}");
    let row = row_of(&state, "t_block");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let detail = detail_text(&state, &frame).join(" ");
    assert!(!detail.contains("do not disturb"), "{detail:?}");
    assert!(!detail.contains("quiet"), "{detail:?}");
}

#[test]
fn another_servers_state_is_never_used_and_older_pushes_are_ignored() {
    let mut state = active_state();
    let mut other = quiet_payload(5, &["t_work"], &[("t_work", 1)]);
    other.boot_id = "boot-other".into();
    assert!(state.receive_quiet(&ClientEndpointId::Local, other));
    assert!(rows(&mut state)
        .iter()
        .all(|(_, symbol, _, text)| symbol == " " && !text.contains('\u{2709}')));
    assert!(!state.active_tab_dnd("t_work"));
    assert!(state.receive_quiet(&ClientEndpointId::Local, quiet_payload(1, &["t_work"], &[])));
    assert!(!state.receive_quiet(&ClientEndpointId::Local, quiet_payload(1, &[], &[])));
    assert!(state.active_tab_dnd("t_work"));
}

#[test]
fn dnd_menu_item_toggles_through_tab_set_dnd_and_hides_without_server_support() {
    let mut state = active_state();
    let open = |state: &mut ClientShellState| {
        state.overlay = None;
        state.compose(COLS, ROWS).expect("composed frame");
        let row = row_of(state, "t_work");
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
    };
    let items = open(&mut state);
    let index = items
        .iter()
        .position(|item| item.label == "Do not disturb")
        .expect("a Do not disturb item");
    let mute = items
        .iter()
        .position(|item| item.action == ClientContextMenuAction::Mute)
        .expect("Mute");
    assert_eq!(index, mute + 1, "right after Mute");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabSetDnd(params)] if params.tab_id == "t_work" && params.dnd
    ));

    assert!(state.receive_quiet(&ClientEndpointId::Local, quiet_payload(1, &["t_work"], &[])));
    let items = open(&mut state);
    let index = items
        .iter()
        .position(|item| item.label == "End do not disturb")
        .expect("an End do not disturb item");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabSetDnd(params)] if params.tab_id == "t_work" && !params.dnd
    ));

    state.set_endpoint_methods(Some(vec!["tab.focus".into(), "tab.set_muted".into()]));
    let items = open(&mut state);
    assert!(!items
        .iter()
        .any(|item| item.action == ClientContextMenuAction::Dnd));
}
