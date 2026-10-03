//! Fork: "Copy session ID" in the tab and pane context menus: the `pane.get`
//! sent when the menu opens, the item appearing only with an id-kind session,
//! and the copy through the clipboard action with the clipboard toast.

use super::*;
use crate::agent_resume::AgentSessionRefKind;
use crate::api::schema::{AgentSessionInfo, Method, ResponseResult};
use crossterm::event::{MouseButton, MouseEventKind};

const CLAUDE_ID: &str = "6f1c2a9e-4b7d-4e21-9c3a-8d05b2e7f413";

fn agent(pane_id: &str, tab_id: &str, focused: bool) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: tab_id.into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused,
        subagents: 0,
    }
}

fn pane(pane_id: &str, focused: bool) -> ClientShellPane {
    ClientShellPane {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: None,
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused,
        right_click_passthrough: false,
    }
}

fn state_with(agents: Vec<ClientShellAgent>) -> ClientShellState {
    let mut projected = snapshot();
    projected.agents = agents;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    state
}

fn right_click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 1,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn right_click_tab(state: &mut ClientShellState) -> ClientShellInput {
    let rect = state
        .hits
        .sidebar_tabs
        .iter()
        .chain(state.hits.tabs.iter())
        .find(|(_, tab_id)| tab_id == "tab_1")
        .map(|(rect, _)| *rect)
        .expect("tab_1 hit");
    right_click(state, rect)
}

/// The `pane.get` the menu sent: (request id, pane id).
fn session_requests(outcome: &ClientShellInput) -> Vec<(String, String)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::PaneGet(target) => Some((request.id.clone(), target.pane_id.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn pane_info(pane_id: &str, session: Option<(AgentSessionRefKind, &str)>) -> ResponseResult {
    let ResponseResult::PaneInfo { mut pane } = pane_scroll_result(0, 0, 0) else {
        unreachable!("pane_scroll_result is a PaneInfo");
    };
    pane.pane_id = pane_id.into();
    pane.scroll = None;
    pane.agent_session = session.map(|(kind, value)| AgentSessionInfo {
        source: "herdr:claude".into(),
        agent: "claude".into(),
        kind,
        value: value.into(),
    });
    ResponseResult::PaneInfo { pane }
}

fn menu_actions(state: &ClientShellState) -> Vec<ClientContextMenuAction> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            menu.items().iter().map(|item| item.action).collect()
        }
        _ => panic!("context menu open"),
    }
}

fn copy_index(state: &ClientShellState) -> Option<usize> {
    menu_actions(state)
        .iter()
        .position(|action| *action == ClientContextMenuAction::CopySessionId)
}

#[test]
fn tab_menu_lists_copy_session_id_after_restart_once_the_id_arrives() {
    let mut state = state_with(vec![agent("pane_1", "tab_1", true)]);
    let opened = right_click_tab(&mut state);
    let requests = session_requests(&opened);
    let [(request_id, pane_id)] = &requests[..] else {
        panic!("one pane.get for the tab's agent, got {requests:?}");
    };
    assert_eq!(pane_id, "pane_1");
    assert_eq!(copy_index(&state), None, "no item before the reply");

    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(pane_info(
            "pane_1",
            Some((AgentSessionRefKind::Id, CLAUDE_ID)),
        )),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    let items = menu_actions(&state);
    let restart = items
        .iter()
        .position(|action| *action == ClientContextMenuAction::RestartAgent)
        .expect("restart item");
    assert_eq!(
        items.get(restart + 1),
        Some(&ClientContextMenuAction::CopySessionId)
    );
    assert_eq!(
        items.get(restart + 2),
        Some(&ClientContextMenuAction::Close)
    );
    let frame = state.compose(106, 20).expect("menu frame");
    assert!(frame_rows(&frame).join("\n").contains("Copy session ID"));
}

#[test]
fn copy_session_id_writes_the_id_to_the_clipboard_with_the_toast() {
    let mut state = state_with(vec![agent("pane_1", "tab_1", true)]);
    let opened = right_click_tab(&mut state);
    let (request_id, _) = session_requests(&opened).remove(0);
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(pane_info(
            "pane_1",
            Some((AgentSessionRefKind::Id, CLAUDE_ID)),
        )),
    );
    let index = copy_index(&state).expect("copy item");

    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(outcome.repaint);
    assert!(state.overlay.is_none());
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::ClipboardWrite(bytes)] if bytes == CLAUDE_ID.as_bytes()
    ));
    assert_eq!(
        state
            .copy_feedback
            .as_ref()
            .map(|feedback| feedback.message.as_str()),
        Some("copied to clipboard")
    );
}

#[test]
fn no_copy_item_without_an_id_kind_session() {
    for session in [
        None,
        Some((AgentSessionRefKind::Path, "/tmp/pi/session.jsonl")),
    ] {
        let mut state = state_with(vec![agent("pane_1", "tab_1", true)]);
        let opened = right_click_tab(&mut state);
        let (request_id, _) = session_requests(&opened).remove(0);
        let (repaint, _) =
            state.handle_endpoint_result("boot-1", &request_id, Ok(pane_info("pane_1", session)));
        assert!(!repaint);
        assert_eq!(copy_index(&state), None);
    }

    // An error reply changes nothing and raises no notice.
    let mut state = state_with(vec![agent("pane_1", "tab_1", true)]);
    let opened = right_click_tab(&mut state);
    let (request_id, _) = session_requests(&opened).remove(0);
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("pane_not_found".into()),
            message: "gone".into(),
        }),
    );
    assert!(!repaint);
    assert!(state.visible_endpoint_notice.is_none());
    assert_eq!(copy_index(&state), None);
}

#[test]
fn tab_without_an_agent_sends_no_lookup_and_a_late_reply_is_ignored() {
    let mut state = state_with(Vec::new());
    let opened = right_click_tab(&mut state);
    assert!(session_requests(&opened).is_empty());
    assert_eq!(copy_index(&state), None);

    let mut state = state_with(vec![agent("pane_1", "tab_1", true)]);
    let opened = right_click_tab(&mut state);
    let (request_id, _) = session_requests(&opened).remove(0);
    state.overlay = None;
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(pane_info(
            "pane_1",
            Some((AgentSessionRefKind::Id, CLAUDE_ID)),
        )),
    );
    assert!(!repaint);
    assert!(state.overlay.is_none());
}

#[test]
fn tab_menu_copies_the_focused_agent_of_a_multi_agent_tab() {
    let mut state = state_with(Vec::new());
    let mut projected = snapshot();
    projected.panes = vec![pane("pane_1", false), pane("pane_2", true)];
    projected.focused_pane_id = Some("pane_2".into());
    projected.agents = vec![
        agent("pane_1", "tab_1", false),
        agent("pane_2", "tab_1", true),
    ];
    state.set_snapshot(Box::new(projected));
    state.compose(106, 20).expect("composed frame");

    let opened = right_click_tab(&mut state);
    let requests = session_requests(&opened);
    assert!(matches!(&requests[..], [(_, pane_id)] if pane_id == "pane_2"));
    let (request_id, _) = requests[0].clone();
    // A reply for another pane is not this menu's.
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(pane_info(
            "pane_1",
            Some((AgentSessionRefKind::Id, "other")),
        )),
    );
    assert!(!repaint);
    assert_eq!(copy_index(&state), None);
}

#[test]
fn the_keyboard_highlight_stays_on_its_item_when_the_copy_item_appears() {
    let mut state = state_with(vec![agent("pane_1", "tab_1", true)]);
    let opened = right_click_tab(&mut state);
    let (request_id, _) = session_requests(&opened).remove(0);
    let close = menu_actions(&state)
        .iter()
        .position(|action| *action == ClientContextMenuAction::Close)
        .expect("close item");
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_mut() else {
        panic!("context menu open");
    };
    menu.highlighted = close;
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(pane_info(
            "pane_1",
            Some((AgentSessionRefKind::Id, CLAUDE_ID)),
        )),
    );
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("context menu open");
    };
    assert_eq!(
        menu.items()[menu.highlighted].action,
        ClientContextMenuAction::Close
    );
}

#[test]
fn pane_menu_lists_and_copies_the_pane_agent_session_id() {
    let mut state = state_with(vec![agent("pane_1", "tab_1", true)]);
    let rect = state.hits.panes[0].rect;
    let opened = right_click(&mut state, rect);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Pane { .. },
            ..
        }))
    ));
    let requests = session_requests(&opened);
    assert!(matches!(&requests[..], [(_, pane_id)] if pane_id == "pane_1"));
    assert_eq!(copy_index(&state), None);
    state.handle_endpoint_result(
        "boot-1",
        &requests[0].0,
        Ok(pane_info(
            "pane_1",
            Some((AgentSessionRefKind::Id, "codex-thread-1")),
        )),
    );
    let items = menu_actions(&state);
    assert_eq!(
        items.last(),
        Some(&ClientContextMenuAction::CopySessionId),
        "after Info pane"
    );

    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(items.len() - 1, &mut outcome);
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::ClipboardWrite(bytes)] if bytes == b"codex-thread-1"
    ));
    assert!(state.copy_feedback.is_some());
}
