//! Fork (menu design): the context menus' shared language
//! (`context_menu_look.rs`): rule rows that are drawn but never selectable,
//! key hints from the effective keybindings, toggles with a stable label
//! and a dot, and destructive items in red.
//!
//! The fixture is sidebar_active.rs's (`t_work` a working Claude tab).

use super::sidebar_active::{
    active_state_with, cell, endpoint_methods, mouse, row_text, tabs_config, COLS, ROWS,
};
use super::*;
use crate::client::shell::context_menu_look::{compact_binding, MenuKind};
use crate::config::BindingConfig;
use crossterm::event::{KeyCode, MouseButton, MouseEventKind};

fn open_tab_menu(
    state: &mut ClientShellState,
    tab_id: &str,
) -> crate::client::frame_output::ComposedFrame {
    state.overlay = None;
    state.open_tab_context_menu(tab_id.into(), 40, 1);
    state.compose(COLS, ROWS).expect("menu frame")
}

fn menu_items(state: &ClientShellState) -> Vec<ClientContextMenuItem> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("context menu"),
    }
}

fn highlighted(state: &ClientShellState) -> usize {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.highlighted,
        _ => panic!("context menu"),
    }
}

fn index_of(state: &ClientShellState, action: ClientContextMenuAction) -> usize {
    menu_items(state)
        .iter()
        .position(|item| item.action == action)
        .expect("menu item")
}

fn key(code: KeyCode) -> RawInputEvent {
    RawInputEvent::Key(crate::input::TerminalKey::new(
        code,
        crossterm::event::KeyModifiers::empty(),
    ))
}

#[test]
fn rule_rows_are_drawn_but_never_selectable() {
    let mut state = active_state_with(ClientShellConfig::from_config(&tabs_config()));
    let frame = open_tab_menu(&mut state, "t_work");
    let items = menu_items(&state);
    assert_eq!(
        state.hits.context_menu_rows.len(),
        items.len(),
        "one hit per item, none for a rule"
    );
    let close = index_of(&state, ClientContextMenuAction::Close);
    let suspend = index_of(&state, ClientContextMenuAction::SuspendAgent);
    assert_eq!(suspend, close + 1, "adjacent items across a rule");
    let close_row = state.hits.context_menu_rows[close].0;
    let suspend_row = state.hits.context_menu_rows[suspend].0;
    assert_eq!(suspend_row.y, close_row.y + 2, "a rule row between them");
    let rule = Rect::new(close_row.x, close_row.y + 1, close_row.width, 1);
    assert!(
        row_text(&frame, rule).starts_with("\u{2500} Agent \u{2500}"),
        "{:?}",
        row_text(&frame, rule)
    );
    let palette = state.config.palette.clone();
    let color = crate::protocol::color_to_u32;
    assert_eq!(cell(&frame, rule.x + 2, rule.y).fg, color(palette.mauve));
    assert_eq!(
        cell(&frame, rule.right() - 1, rule.y).fg,
        color(palette.surface1)
    );

    // Hovering the rule moves nothing; a click on it picks nothing and
    // keeps the menu.
    mouse(&mut state, MouseEventKind::Moved, rule.x + 3, rule.y);
    assert_eq!(highlighted(&state), 0);
    let clicked = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        rule.x + 3,
        rule.y,
    );
    assert!(endpoint_methods(&clicked).is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(_))
    ));

    // The keyboard steps over it: Close, then Suspend agent.
    for _ in 0..close {
        state.handle_raw_events(vec![key(KeyCode::Down)]);
    }
    assert_eq!(highlighted(&state), close);
    state.handle_raw_events(vec![key(KeyCode::Down)]);
    assert_eq!(highlighted(&state), suspend);
    state.handle_raw_events(vec![key(KeyCode::Up)]);
    assert_eq!(highlighted(&state), close);

    // A click outside the menu still closes it.
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), 104, 40);
    assert!(state.overlay.is_none());
}

#[test]
fn the_pane_menu_starts_with_a_rule_and_highlights_its_first_item() {
    let mut state = active_state_with(ClientShellConfig::from_config(&tabs_config()));
    state.open_pane_context_menu("p_work".into(), 40, 1);
    let frame = state.compose(COLS, ROWS).expect("menu frame");
    let items = menu_items(&state);
    assert_eq!(items[0].action, ClientContextMenuAction::SplitRight);
    assert_eq!(highlighted(&state), 0);
    let first = state.hits.context_menu_rows[0].0;
    let rule = Rect::new(first.x, first.y - 1, first.width, 1);
    assert!(row_text(&frame, rule).starts_with("\u{2500} Layout "));
    // Close pane is last, behind an untitled rule, in red.
    let close = items.len() - 1;
    assert_eq!(items[close].action, ClientContextMenuAction::ClosePane);
    let close_row = state.hits.context_menu_rows[close].0;
    let above = state.hits.context_menu_rows[close - 1].0;
    assert_eq!(close_row.y, above.y + 2);
    let line = Rect::new(close_row.x, close_row.y - 1, close_row.width, 1);
    assert!(row_text(&frame, line).chars().all(|c| c == '\u{2500}'));
    assert!(
        state.hits.context_menu_rows[0].0.width + 2 <= 34,
        "the menu stays narrow"
    );
}

#[test]
fn key_hints_follow_the_effective_keybindings_and_unbound_items_show_none() {
    let mut config = tabs_config();
    config.keys.new_tab = BindingConfig::one("alt+t");
    config.keys.toggle_info_pane = BindingConfig::one("alt+n");
    config.keys.close_tab = BindingConfig::one("prefix+shift+x");
    config.keys.rename_tab = BindingConfig::empty();
    let mut state = active_state_with(ClientShellConfig::from_config(&config));
    let frame = open_tab_menu(&mut state, "t_work");
    let text = |action| {
        let index = index_of(&state, action);
        row_text(&frame, state.hits.context_menu_rows[index].0)
    };
    let new_tab = text(ClientContextMenuAction::NewTab);
    assert!(new_tab.starts_with(" + New tab"), "{new_tab:?}");
    assert!(new_tab.ends_with("\u{2325}t  "), "{new_tab:?}");
    assert!(text(ClientContextMenuAction::ToggleInfoPane).ends_with("\u{2325}n  "));
    assert!(text(ClientContextMenuAction::Close).ends_with("^b X  "));
    assert_eq!(
        text(ClientContextMenuAction::Rename).trim_end(),
        " \u{270E} Rename",
        "unbound: no hint"
    );
    // The hint is dim.
    let index = index_of(&state, ClientContextMenuAction::ToggleInfoPane);
    let row = state.hits.context_menu_rows[index].0;
    assert_eq!(
        cell(&frame, row.right() - 3, row.y).fg,
        crate::protocol::color_to_u32(state.config.palette.overlay0)
    );

    // A rebinding shows on the next open.
    let mut config = tabs_config();
    config.keys.new_tab = BindingConfig::one("ctrl+alt+y");
    state.config = ClientShellConfig::from_config(&config);
    let frame = open_tab_menu(&mut state, "t_work");
    let index = index_of(&state, ClientContextMenuAction::NewTab);
    assert!(row_text(&frame, state.hits.context_menu_rows[index].0).ends_with("^\u{2325}y  "));
}

#[test]
fn compact_bindings_use_the_primary_prefix() {
    let ctrl_b = Some((KeyCode::Char('b'), crossterm::event::KeyModifiers::CONTROL));
    let ctrl_a = Some((KeyCode::Char('a'), crossterm::event::KeyModifiers::CONTROL));
    assert_eq!(
        compact_binding("alt+t", ctrl_b).as_deref(),
        Some("\u{2325}t")
    );
    assert_eq!(
        compact_binding("prefix+shift+x", ctrl_b).as_deref(),
        Some("^b X")
    );
    assert_eq!(compact_binding("prefix+v", ctrl_a).as_deref(), Some("^a v"));
    assert_eq!(
        compact_binding("prefix+minus", ctrl_b).as_deref(),
        Some("^b -")
    );
    assert_eq!(compact_binding("not a key", ctrl_b), None);
}

#[test]
fn toggles_keep_their_label_and_show_the_state_with_a_dot() {
    let mut state = active_state_with(ClientShellConfig::from_config(&tabs_config()));
    open_tab_menu(&mut state, "t_work");
    let kinds = |state: &ClientShellState| {
        menu_items(state)
            .into_iter()
            .filter(|item| {
                matches!(
                    item.action,
                    ClientContextMenuAction::Important
                        | ClientContextMenuAction::Pin
                        | ClientContextMenuAction::Mute
                        | ClientContextMenuAction::Dnd
                )
            })
            .map(|item| (item.label, item.look.glyph, item.look.kind))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        kinds(&state),
        [
            ("Important", "\u{2606}", MenuKind::Star(false)),
            ("Pin", "\u{25CB}", MenuKind::Toggle(false)),
            ("Mute notifications", "\u{25CB}", MenuKind::Toggle(false)),
            ("Do not disturb", "\u{25CB}", MenuKind::Toggle(false)),
        ]
    );

    // Pinned, muted, in DND and important: same labels, filled dots, `on`.
    for tab in &mut state.snapshot.as_deref_mut().expect("snapshot").tabs {
        if tab.tab_id == "t_work" {
            tab.important = true;
        }
    }
    let ids = vec!["t_work".to_string()];
    state.receive_tab_pins(
        &ClientEndpointId::Local,
        crate::server::headless::tab_pins::TabPinsPayload {
            boot_id: "boot-1".into(),
            revision: 1,
            tab_ids: ids.clone(),
        },
    );
    state.receive_tab_mutes(
        &ClientEndpointId::Local,
        crate::server::headless::tab_mutes::TabMutesPayload {
            boot_id: "boot-1".into(),
            revision: 1,
            tab_ids: ids,
        },
    );
    state.receive_quiet(
        &ClientEndpointId::Local,
        serde_json::from_value(serde_json::json!({
            "boot_id": "boot-1",
            "revision": 1,
            "dnd_tab_ids": ["t_work"],
        }))
        .expect("quiet payload"),
    );
    let frame = open_tab_menu(&mut state, "t_work");
    assert_eq!(
        kinds(&state),
        [
            ("Important", "\u{2605}", MenuKind::Star(true)),
            ("Pin", "\u{25CF}", MenuKind::Toggle(true)),
            ("Mute notifications", "\u{25CF}", MenuKind::Toggle(true)),
            ("Do not disturb", "\u{25CF}", MenuKind::Toggle(true)),
        ]
    );
    let palette = state.config.palette.clone();
    let color = crate::protocol::color_to_u32;
    let pin = index_of(&state, ClientContextMenuAction::Pin);
    let row = state.hits.context_menu_rows[pin].0;
    assert!(row_text(&frame, row).ends_with("on  "));
    assert_eq!(cell(&frame, row.x + 1, row.y).fg, color(palette.green));
    assert_eq!(
        cell(&frame, row.right() - 4, row.y).fg,
        color(palette.green)
    );
    let star = index_of(&state, ClientContextMenuAction::Important);
    let row = state.hits.context_menu_rows[star].0;
    assert_eq!(cell(&frame, row.x + 1, row.y).fg, color(palette.yellow));
}

#[test]
fn destructive_items_are_red_with_a_red_cursor_bar() {
    let mut state = active_state_with(ClientShellConfig::from_config(&tabs_config()));
    let frame = open_tab_menu(&mut state, "t_work");
    let palette = state.config.palette.clone();
    let color = crate::protocol::color_to_u32;
    let close = index_of(&state, ClientContextMenuAction::Close);
    assert_eq!(menu_items(&state)[close].look.kind, MenuKind::Danger);
    let row = state.hits.context_menu_rows[close].0;
    assert_eq!(cell(&frame, row.x + 3, row.y).fg, color(palette.red));
    assert_eq!(cell(&frame, row.x + 3, row.y).bg, color(palette.panel_bg));
    // The normal cursor is the accent bar.
    let first = state.hits.context_menu_rows[0].0;
    assert_eq!(cell(&frame, first.x + 3, first.y).bg, color(palette.accent));

    for _ in 0..close {
        state.handle_raw_events(vec![key(KeyCode::Down)]);
    }
    let frame = state.compose(COLS, ROWS).expect("menu frame");
    let row = state.hits.context_menu_rows[close].0;
    assert_eq!(cell(&frame, row.x + 3, row.y).bg, color(palette.red));
    assert_eq!(
        cell(&frame, row.x + 3, row.y).fg,
        color(super::super::panel_contrast_fg(&palette))
    );
}

#[test]
fn a_narrow_terminal_clamps_the_menu_and_keeps_the_labels() {
    let mut state = active_state_with(ClientShellConfig::from_config(&tabs_config()));
    state.overlay = None;
    state.open_tab_context_menu("t_work".into(), 40, 1);
    let frame = state.compose(24, ROWS).expect("menu frame");
    let rows = state.hits.context_menu_rows.clone();
    assert!(!rows.is_empty());
    for (rect, _) in &rows {
        assert!(rect.right() <= 23, "inside the screen: {rect:?}");
    }
    let new_tab = row_text(&frame, rows[0].0);
    assert!(new_tab.starts_with(" + New tab"), "{new_tab:?}");
}

#[test]
fn a_toggles_state_never_changes_the_menu_width() {
    let mut state = active_state_with(ClientShellConfig::from_config(&tabs_config()));
    let width = |state: &mut ClientShellState| {
        state.overlay = None;
        state.open_pane_context_menu("p_work".into(), 40, 1);
        let frame = state.compose(COLS, ROWS).expect("menu frame");
        let index = index_of(state, ClientContextMenuAction::ToggleRightClickPassthrough);
        let row = state.hits.context_menu_rows[index].0;
        (state.hits.context_menu_area.width, row_text(&frame, row))
    };
    let (off_width, off) = width(&mut state);
    assert!(
        off.starts_with(" \u{25CB} Send right-clicks to pane"),
        "{off:?}"
    );
    for pane in &mut state.snapshot.as_deref_mut().expect("snapshot").panes {
        if pane.pane_id == "p_work" {
            pane.right_click_passthrough = true;
        }
    }
    let (on_width, on) = width(&mut state);
    assert!(
        on.starts_with(" \u{25CF} Send right-clicks to pane"),
        "{on:?}"
    );
    assert_eq!(on_width, off_width, "the dot carries the state");
    assert!(on_width <= 34);
}
