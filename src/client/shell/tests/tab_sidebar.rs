use super::super::tab_sidebar::{FOLD_ALL_LABEL, UNFOLD_ALL_LABEL};
use super::*;
use crate::config::{Config, SidebarLayoutConfig};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config
}

fn tab(
    tab_id: &str,
    workspace_id: &str,
    number: usize,
    label: &str,
    focused: bool,
    status: AgentStatus,
) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: workspace_id.into(),
        number,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused,
        agent_status: status,
        color: None,
    }
}

/// Two spaces, three tabs: the endpoint emits `tabs` space by space and then in
/// tab order, and the sidebar must mirror that order without regrouping.
fn two_space_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.active_tab_id = "tab_3".into();
    second.number = 2;
    second.label = "other".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    snapshot.tabs = vec![
        tab("tab_1", "ws_1", 1, "reviewer", true, AgentStatus::Working),
        tab("tab_2", "ws_1", 2, "planner", false, AgentStatus::Blocked),
        tab("tab_3", "ws_2", 1, "notes", false, AgentStatus::Idle),
    ];
    snapshot
}

fn row_text(frame: &FrameData, rect: ratatui::layout::Rect) -> String {
    (rect.x..rect.right())
        .map(|x| {
            frame.cells[(rect.y * frame.width + x) as usize]
                .symbol
                .as_str()
        })
        .collect::<String>()
}

#[test]
fn tabs_layout_lists_every_tab_in_order_and_hides_space_rows() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(two_space_snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");

    // Only group headers register as space hits (ws_2 is a group; ws_1 is the bucket).
    let header_ids = state
        .hits
        .workspaces
        .iter()
        .map(|hit| hit.workspace_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(header_ids, ["ws_2"]);
    assert_eq!(state.hits.new_workspace.width, 0, "no new-space button");
    assert_eq!(
        state.hits.sidebar_section_divider,
        ratatui::layout::Rect::default()
    );
    let ids = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(_, id)| id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["tab_1", "tab_2", "tab_3"]);
    let rows = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| row_text(&frame, *rect))
        .collect::<Vec<_>>();
    assert!(rows[0].contains("reviewer"), "{rows:?}");
    assert!(rows[1].contains("planner"), "{rows:?}");
    assert!(rows[2].contains("notes"), "{rows:?}");
    assert!(
        rows[0]
            .trim_start()
            .starts_with(status_dot(AgentStatus::Working)),
        "{rows:?}"
    );
    assert!(state.hits.agent_body.height >= 3);
    assert_eq!(
        state.hits.sidebar_tabs[0].0.y, 1,
        "the list starts right under the one-row toolbar"
    );
}

#[test]
fn tabs_layout_rows_focus_on_click_and_open_the_tab_menu_on_right_click() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(two_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let (rect, _) = state.hits.sidebar_tabs[2];
    let pressed = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(
        pressed.actions.is_empty(),
        "focus waits for release (drag-safe)"
    );
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("sidebar tab click should focus through the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_3"
    ));

    let (rect, _) = state.hits.sidebar_tabs[1];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { ref tab_id, ref workspace_id, .. },
            ..
        })) if tab_id == "tab_2" && workspace_id == "ws_1"
    ));
}

#[test]
fn spaces_layout_registers_no_sidebar_tab_rows() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(two_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    assert!(state.hits.sidebar_tabs.is_empty());
    assert!(!state.hits.workspaces.is_empty());
}

#[test]
fn live_config_reload_switches_sidebar_layout() {
    let mut shell = ClientShellConfig::from_config(&Config::default());
    assert_eq!(shell.sidebar_layout, SidebarLayoutConfig::Spaces);
    let diagnostics = shell.apply_live_config(&tabs_config(), &[], &[]);
    assert!(diagnostics.is_empty());
    assert_eq!(shell.sidebar_layout, SidebarLayoutConfig::Tabs);
}

#[test]
fn tabs_layout_drops_the_horizontal_tab_bar() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(two_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    assert!(state.hits.tabs.is_empty());
    assert_eq!(state.hits.new_tab, ratatui::layout::Rect::default());
    let layout = state.layout(106, 20);
    assert!(layout.tab_bar.is_empty());
    assert_eq!(layout.pane_surface.height, 20);

    let spaces = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    assert!(!spaces.layout(106, 20).tab_bar.is_empty());
}

fn tab_action(state: &mut ClientShellState, action: crate::input::KeybindAction) -> Option<String> {
    let mut outcome = ClientShellInput::default();
    state.record_binding(crate::input::KeybindMatch::Action(action), &mut outcome);
    outcome.actions.iter().find_map(|action| match action {
        ClientShellAction::Endpoint { request, .. } => match &request.method {
            crate::api::schema::Method::TabFocus(target) => Some(target.tab_id.clone()),
            _ => None,
        },
        _ => None,
    })
}

#[test]
fn tabs_layout_cycles_next_and_previous_across_spaces() {
    use crate::input::KeybindAction;
    let mut snapshot = two_space_snapshot();
    // Focus the last tab of the first space.
    snapshot.focused_tab_id = Some("tab_2".into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_2";
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    assert_eq!(
        tab_action(&mut state, KeybindAction::NextTab).as_deref(),
        Some("tab_3"),
        "next crosses into the second space"
    );

    snapshot.focused_tab_id = Some("tab_1".into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_1";
    }
    state.set_snapshot(Box::new(snapshot.clone()));
    state.compose(106, 20).expect("composed frame");
    assert_eq!(
        tab_action(&mut state, KeybindAction::PreviousTab).as_deref(),
        Some("tab_3"),
        "previous wraps to the end of the whole list"
    );

    // The spaces layout keeps cycling inside the focused space.
    let mut spaces = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    spaces.set_snapshot(Box::new(snapshot));
    spaces.set_pane_surface(surface());
    spaces.compose(106, 20).expect("composed frame");
    assert_eq!(
        tab_action(&mut spaces, KeybindAction::PreviousTab).as_deref(),
        Some("tab_2")
    );
}

fn agent(pane_id: &str, tab_id: &str, status: AgentStatus) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: tab_id.into(),
        name: Some("reviewer".into()),
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    }
}

fn endpoint_methods(outcome: &ClientShellInput) -> Vec<&crate::api::schema::Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .collect()
}

#[test]
fn tab_menu_offers_suspend_for_live_agents_and_activate_for_parked_ones() {
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![
        agent("pane_1", "tab_1", AgentStatus::Working),
        agent("pane_2", "tab_2", AgentStatus::Suspended),
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let (rect, _) = state.hits.sidebar_tabs[0];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let items = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("tab context menu"),
    };
    assert!(items
        .iter()
        .any(|item| item.action == ClientContextMenuAction::SuspendAgent));
    assert!(!items
        .iter()
        .any(|item| item.action == ClientContextMenuAction::ActivateAgent));

    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[1];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 20).expect("tab context menu");
    let activate_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::ActivateAgent)
            .expect("activate item"),
        _ => panic!("tab context menu"),
    };
    let row = state.hits.context_menu_rows[activate_index].0;
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 1,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::AgentActivate(params) if params.target == "pane_2"
    )));

    // A tab without an agent gets neither item.
    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[2];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let items = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("tab context menu"),
    };
    assert!(!items.iter().any(|item| matches!(
        item.action,
        ClientContextMenuAction::SuspendAgent | ClientContextMenuAction::ActivateAgent
    )));
}

#[test]
fn toggle_agent_suspend_targets_the_focused_pane_by_its_status() {
    use crate::input::{KeybindAction, KeybindMatch};
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![agent("pane_1", "tab_1", AgentStatus::Idle)];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ToggleAgentSuspend),
        &mut outcome,
    );
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::AgentSuspend(params) if params.target == "pane_1"
    )));

    snapshot.agents[0].agent_status = AgentStatus::Suspended;
    state.set_snapshot(Box::new(snapshot));
    state.compose(106, 20).expect("composed frame");
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ToggleAgentSuspend),
        &mut outcome,
    );
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::AgentActivate(params) if params.target == "pane_1"
    )));

    let bound: Config = toml::from_str("[keys]\ntoggle_agent_suspend = \"alt+s\"").unwrap();
    assert!(!bound
        .live_keybinds_with_diagnostics()
        .map(|(keybinds, _)| keybinds.keybinds.toggle_agent_suspend.bindings.is_empty())
        .unwrap_or(true));
}

fn tab_menu_items(state: &mut ClientShellState, row: usize) -> Vec<ClientContextMenuItem> {
    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[row];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("tab context menu"),
    }
}

#[test]
fn tab_menu_offers_restart_only_for_live_agents() {
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![
        agent("pane_1", "tab_1", AgentStatus::Idle),
        agent("pane_2", "tab_2", AgentStatus::Suspended),
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());

    let live = tab_menu_items(&mut state, 0);
    let suspend_index = live
        .iter()
        .position(|item| item.action == ClientContextMenuAction::SuspendAgent)
        .expect("suspend item");
    let restart_index = live
        .iter()
        .position(|item| item.action == ClientContextMenuAction::RestartAgent)
        .expect("restart item for a live agent");
    assert_eq!(restart_index, suspend_index + 1, "next to Suspend agent");
    assert_eq!(live[restart_index].label, "Restart agent");

    state.compose(106, 20).expect("tab context menu");
    let row = state.hits.context_menu_rows[restart_index].0;
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        row.x + 1,
        row.y,
    )]);
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::AgentRestart(params) if params.target == "pane_1"
    )));

    let parked = tab_menu_items(&mut state, 1);
    assert!(!parked
        .iter()
        .any(|item| item.action == ClientContextMenuAction::RestartAgent));
    assert!(parked
        .iter()
        .any(|item| item.action == ClientContextMenuAction::ActivateAgent));

    let plain = tab_menu_items(&mut state, 2);
    assert!(!plain
        .iter()
        .any(|item| item.action == ClientContextMenuAction::RestartAgent));
}

#[test]
fn toggle_agent_suspend_on_a_working_agent_surfaces_the_refusal() {
    use crate::input::{KeybindAction, KeybindMatch};
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![agent("pane_1", "tab_1", AgentStatus::Working)];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ToggleAgentSuspend),
        &mut outcome,
    );
    let request_id = outcome
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::AgentSuspend(params) if params.target == "pane_1" => {
                    Some(request.id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("the toggle still asks the server to suspend a working agent");

    let (repaint, _) = state.handle_endpoint_result(
        &snapshot.boot_id,
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("agent_working".into()),
            message: "agent pane_1 is working; wait for idle or blocked-free state".into(),
        }),
    );
    assert!(repaint);
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("rejected suspend notice");
    assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Rejected);
    assert_eq!(notice.key.code, "agent.suspend:agent_working");
    assert_eq!(
        notice.body,
        "agent pane_1 is working; wait for idle or blocked-free state"
    );
}

#[test]
fn restart_agent_binding_requests_a_restart_and_surfaces_a_refusal() {
    use crate::input::{KeybindAction, KeybindMatch};
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![agent("pane_1", "tab_1", AgentStatus::Working)];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::RestartAgent),
        &mut outcome,
    );
    let request_id = outcome
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::AgentRestart(params) if params.target == "pane_1" => {
                    Some(request.id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("agent.restart request for the focused pane");

    // The server's refusal takes the generic rejected-action path.
    let (repaint, _) = state.handle_endpoint_result(
        &snapshot.boot_id,
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("agent_working".into()),
            message: "agent pane_1 is working; wait for idle or blocked-free state".into(),
        }),
    );
    assert!(repaint);
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("rejected restart notice");
    assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Rejected);
    assert_eq!(notice.key.code, "agent.restart:agent_working");

    // A suspended agent is activated, not restarted.
    snapshot.agents[0].agent_status = AgentStatus::Suspended;
    state.set_snapshot(Box::new(snapshot));
    state.compose(106, 20).expect("composed frame");
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::RestartAgent),
        &mut outcome,
    );
    assert!(endpoint_methods(&outcome).is_empty());

    assert!(!Config::default().keys.restart_agent.has_values());
    let bound: Config = toml::from_str("[keys]\nrestart_agent = \"alt+r\"").unwrap();
    assert!(!bound
        .live_keybinds_with_diagnostics()
        .map(|(keybinds, _)| keybinds.keybinds.restart_agent.bindings.is_empty())
        .unwrap_or(true));
}

fn frame_text(frame: &FrameData, rect: ratatui::layout::Rect) -> String {
    (rect.y..rect.bottom())
        .map(|y| {
            (rect.x..rect.right())
                .map(|x| frame.cells[(y * frame.width + x) as usize].symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn suspended_state() -> ClientShellState {
    let mut config = tabs_config();
    config.keys.toggle_agent_suspend = crate::config::BindingConfig::one("alt+s");
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![agent("pane_1", "tab_1", AgentStatus::Suspended)];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(wide_surface());
    state
}

#[test]
fn suspended_pane_shows_a_card_instead_of_the_shell_and_hides_the_cursor() {
    let mut state = suspended_state();
    let frame = state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].inner_rect;
    let text = frame_text(&frame, pane);
    assert!(text.contains("suspended"), "{text}");
    assert!(text.contains("reviewer"), "{text}");
    assert!(text.contains("alt+s to activate"), "{text}");
    assert!(
        !text.contains("SHELL SCROLLBACK"),
        "old screen must not bleed through: {text}"
    );
    assert!(
        frame.cursor.is_none(),
        "cursor must not show inside a suspended pane"
    );

    // The spaces layout leaves the pane alone.
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![agent("pane_1", "tab_1", AgentStatus::Suspended)];
    let mut spaces = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    spaces.set_snapshot(Box::new(snapshot));
    spaces.set_pane_surface(wide_surface());
    let frame = spaces.compose(106, 20).expect("composed frame");
    let pane = spaces.hits.panes[0].inner_rect;
    let text = frame_text(&frame, pane);
    assert!(!text.contains("suspended"), "{text}");
    assert!(text.contains("SHELL SCROLLBACK"), "{text}");
}

#[test]
fn suspended_pane_swallows_input_but_the_toggle_binding_still_activates() {
    use crossterm::event::KeyCode;
    let mut state = suspended_state();
    state.compose(106, 20).expect("composed frame");

    let outcome = state.handle_raw_events(vec![
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )),
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Char('l'),
            KeyModifiers::empty(),
        )),
        RawInputEvent::Text(crate::input::TextCommit::new("ls")),
        RawInputEvent::Paste("rm -rf /".into()),
    ]);
    assert!(
        !outcome
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })),
        "no input may reach a suspended pane: {:?}",
        outcome.requests.len()
    );

    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Char('s'), KeyModifiers::ALT),
    )]);
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::AgentActivate(params) if params.target == "pane_1"
    )));
}

#[test]
fn tabs_layout_ignores_pane_topology_actions_and_routes_pane_right_click_to_the_tab_menu() {
    use crate::input::{KeybindAction, KeybindMatch};
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(two_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    for action in [
        KeybindAction::SplitVertical,
        KeybindAction::SplitHorizontal,
        KeybindAction::Zoom,
        KeybindAction::ClosePane,
        KeybindAction::EnterResizeMode,
    ] {
        let mut outcome = ClientShellInput::default();
        state.record_binding(KeybindMatch::Action(action), &mut outcome);
        assert!(outcome.actions.is_empty(), "{action:?} must be inert");
        assert_eq!(
            state.mode,
            ClientShellMode::Terminal,
            "{action:?} must not change mode"
        );
    }

    let pane = state.hits.panes[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: pane.x + 1,
        row: pane.y + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { ref tab_id, .. },
            ..
        })) if tab_id == "tab_1"
    ));
}

/// A 60x12 single-pane surface so the card has room; the shared fixture is 4x2.
fn wide_surface() -> PaneSurfaceFrame {
    let mut frame = surface();
    let lines = vec![format!("{:<60}", "SHELL SCROLLBACK LINE"); 12];
    frame.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &ratatui::buffer::Buffer::with_lines(lines),
        Some(crate::protocol::CursorState {
            x: 3,
            y: 5,
            visible: true,
            shape: 2,
        }),
        &[],
    );
    let rect = SurfaceRect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    frame.panes[0].rect = rect;
    frame.panes[0].inner_rect = rect;
    frame
}

#[test]
fn suspended_pane_shell_output_takes_the_full_compose_path_so_the_card_stays_on_top() {
    let mut state = suspended_state();
    let composed = state.compose(106, 20).expect("initial composed frame");
    let pane = state.hits.panes[0].inner_rect;
    assert!(frame_text(&composed, pane).contains("suspended"));

    // The parked shell redraws its prompt: a row patch against the current surface.
    let mut updated_pane = state.pane_surface.as_ref().unwrap().panes[0].clone();
    updated_pane.content_revision = 2;
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 5,
            cells: vec![
                crate::protocol::CellData {
                    symbol: "$".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                };
                60
            ],
        }],
        panes: vec![updated_pane],
        cursor: None,
    };
    let outcome = state.apply_pane_surface_patch(patch);
    match outcome {
        ClientPaneSurfacePatchOutcome::Applied(None) => {}
        ClientPaneSurfacePatchOutcome::Applied(Some(_)) => {
            panic!("a suspended pane must never take the retained fast path")
        }
        ClientPaneSurfacePatchOutcome::Rejected => panic!("patch should apply"),
    }
    assert_eq!(state.pane_surface.as_ref().unwrap().surface_revision, 2);

    let recomposed = state.compose(106, 20).expect("recomposed frame");
    let text = frame_text(&recomposed, pane);
    assert!(text.contains("suspended"), "{text}");
    assert!(
        !text.contains("$$$"),
        "shell rows must stay under the card: {text}"
    );
}

#[test]
fn tabs_layout_switch_tab_indexes_the_whole_list() {
    use crate::input::KeybindAction;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(two_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    assert_eq!(
        tab_action(&mut state, KeybindAction::SwitchTab(2)).as_deref(),
        Some("tab_3"),
        "third row is the second space's tab"
    );
    assert_eq!(tab_action(&mut state, KeybindAction::SwitchTab(7)), None);

    let mut spaces = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    spaces.set_snapshot(Box::new(two_space_snapshot()));
    spaces.set_pane_surface(surface());
    spaces.compose(106, 20).expect("composed frame");
    assert_eq!(tab_action(&mut spaces, KeybindAction::SwitchTab(2)), None);
}

#[test]
fn tab_rows_end_with_the_agent_glyph() {
    let mut snapshot = two_space_snapshot();
    let mut codex = agent("pane_2", "tab_2", AgentStatus::Idle);
    codex.agent = Some("codex".into());
    let mut other = agent("pane_3", "tab_3", AgentStatus::Idle);
    other.agent = Some("gemini".into());
    snapshot.agents = vec![agent("pane_1", "tab_1", AgentStatus::Working), codex, other];
    snapshot.tabs.push(tab(
        "tab_4",
        "ws_2",
        2,
        "shell",
        false,
        AgentStatus::Unknown,
    ));
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    let rows = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| row_text(&frame, *rect).trim_end().to_string())
        .collect::<Vec<_>>();
    assert!(rows[0].ends_with('\u{29C6}'), "claude: {rows:?}");
    assert!(rows[1].ends_with('\u{29C7}'), "codex: {rows:?}");
    assert!(rows[2].ends_with('\u{237E}'), "other agent: {rows:?}");
    assert!(rows[3].ends_with('\u{29C5}'), "plain shell: {rows:?}");
    assert!(rows[0].contains("reviewer"), "{rows:?}");

    let mut config = tabs_config();
    config
        .ui
        .tab_agent_glyphs
        .insert("claude".into(), "C".into());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[0];
    assert!(row_text(&frame, rect).trim_end().ends_with('C'));
}

/// The glyph cell (fg, bg) of each tab row, in row order.
fn glyph_cells(state: &ClientShellState, frame: &FrameData) -> Vec<(String, u32, u32)> {
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| {
            // " <icon> <label>...<glyph> ": the glyph sits one cell in from the right.
            let x = rect.right() - 2;
            let cell = &frame.cells[(rect.y * frame.width + x) as usize];
            (cell.symbol.clone(), cell.fg, cell.bg)
        })
        .collect()
}

fn glyph_color_state(config: &Config, focused_tab: &str) -> (ClientShellState, FrameData) {
    let mut snapshot = two_space_snapshot();
    // tab_1 and tab_2 run claude, tab_3 is a plain shell.
    snapshot.agents = vec![
        agent("pane_1", "tab_1", AgentStatus::Working),
        agent("pane_2", "tab_2", AgentStatus::Idle),
    ];
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == focused_tab;
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    (state, frame)
}

#[test]
fn focused_tab_glyph_wears_the_agent_brand_color() {
    use crate::protocol::color_to_u32;
    use ratatui::style::Color;
    let (state, frame) = glyph_color_state(&tabs_config(), "tab_1");
    let palette = &state.config.palette;
    let orange = color_to_u32(Color::Rgb(0xD9, 0x77, 0x57));
    let cells = glyph_cells(&state, &frame);
    assert_eq!(cells[0].0, "\u{29C6}", "{cells:?}");
    assert_eq!(cells[0].1, orange, "focused claude glyph is orange");
    assert_eq!(
        cells[0].2,
        color_to_u32(palette.active_row_bg),
        "keeps the selected row background"
    );
    assert_eq!(cells[1].0, "\u{29C6}", "{cells:?}");
    assert_ne!(
        cells[1].1, orange,
        "unfocused claude glyph stays monochrome"
    );
    assert_eq!(cells[1].1, color_to_u32(palette.overlay0));

    // A focused shell tab keeps the monochrome glyph.
    let (state, frame) = glyph_color_state(&tabs_config(), "tab_3");
    let cells = glyph_cells(&state, &frame);
    assert_eq!(cells[2].0, "\u{29C5}", "{cells:?}");
    assert_eq!(cells[2].1, color_to_u32(state.config.palette.overlay0));
    assert_eq!(cells[2].2, color_to_u32(state.config.palette.active_row_bg));
    assert_eq!(cells[0].1, color_to_u32(state.config.palette.overlay0));
}

#[test]
fn tab_agent_glyph_color_overrides_win_and_invalid_values_stay_monochrome() {
    use crate::protocol::color_to_u32;
    use ratatui::style::Color;
    let mut config = tabs_config();
    config
        .ui
        .tab_agent_glyph_colors
        .insert("claude".into(), "magenta".into());
    let (state, frame) = glyph_color_state(&config, "tab_1");
    assert_eq!(
        glyph_cells(&state, &frame)[0].1,
        color_to_u32(Color::Magenta)
    );

    let mut config = tabs_config();
    config
        .ui
        .tab_agent_glyph_colors
        .insert("claude".into(), "not-a-color".into());
    let (state, frame) = glyph_color_state(&config, "tab_1");
    assert_eq!(
        glyph_cells(&state, &frame)[0].1,
        color_to_u32(state.config.palette.overlay0),
        "an invalid color keeps the glyph monochrome"
    );
    assert!(config
        .collect_diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.contains("ui.tab_agent_glyph_colors.claude")));

    // A live reload reports the same diagnostic and applies the fallback.
    let mut shell = ClientShellConfig::from_config(&tabs_config());
    let diagnostics = shell.apply_live_config(&config, &[], &[]);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ui.tab_agent_glyph_colors.claude")),
        "{diagnostics:?}"
    );
    assert_eq!(
        crate::config::tab_agent_glyph_color(&shell.tab_agent_glyph_colors, "claude"),
        None
    );
}

#[test]
fn suspended_pane_mouse_press_starts_no_gesture_and_no_selection() {
    let mut state = suspended_state();
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].inner_rect;
    let mut events = Vec::new();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        events.push(RawInputEvent::Mouse(MouseEvent {
            kind,
            column: pane.x + 3,
            row: pane.y + 2,
            modifiers: KeyModifiers::empty(),
        }));
    }
    let outcome = state.handle_raw_events(events);
    assert!(
        !outcome
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })),
        "no mouse input may reach a suspended pane"
    );
    assert!(
        state.pane_mouse_gesture.is_none(),
        "no gesture on a locked pane"
    );
    assert!(state.selection.is_none(), "no selection on a hidden shell");
    // Focusing the pane by click is still fine.
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    )));
}

#[test]
fn tab_menu_acts_on_the_pane_captured_when_it_opened() {
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![agent("pane_2", "tab_2", AgentStatus::Suspended)];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[1];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    // The snapshot changes under the open menu: another agent now leads the tab.
    let mut newcomer = agent("pane_9", "tab_2", AgentStatus::Working);
    newcomer.pane_id = "pane_9".into();
    snapshot.agents.insert(0, newcomer);
    state.set_snapshot(Box::new(snapshot));
    state.compose(106, 20).expect("menu open");
    let activate_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::ActivateAgent)
            .expect("activate item"),
        _ => panic!("tab context menu"),
    };
    let row = state.hits.context_menu_rows[activate_index].0;
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 1,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::AgentActivate(params) if params.target == "pane_2"
    )));
}

fn pane(pane_id: &str, workspace_id: &str, tab_id: &str) -> ClientShellPane {
    ClientShellPane {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        label: None,
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: false,
        right_click_passthrough: false,
    }
}

/// Bucket ws_1 (reviewer, planner), group "api" ws_2 (notes), group "infra" ws_3 (deploy).
fn three_space_snapshot() -> ClientShellSnapshot {
    let mut snapshot = two_space_snapshot();
    snapshot.workspaces[1].label = "api".into();
    let mut third = snapshot.workspaces[1].clone();
    third.workspace_id = "ws_3".into();
    third.active_tab_id = "tab_4".into();
    third.number = 3;
    third.label = "infra".into();
    snapshot.workspaces.push(third);
    snapshot.tabs.push(tab(
        "tab_4",
        "ws_3",
        1,
        "deploy",
        false,
        AgentStatus::Working,
    ));
    snapshot.panes = vec![
        pane("pane_1", "ws_1", "tab_1"),
        pane("pane_2", "ws_1", "tab_2"),
        pane("pane_3", "ws_2", "tab_3"),
        pane("pane_4", "ws_3", "tab_4"),
    ];
    snapshot
}

fn grouped_state() -> ClientShellState {
    let mut config = tabs_config();
    config.keys.move_tab_to_group = crate::config::BindingConfig::one("alt+g");
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(three_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("composed frame");
    state
}

fn listed_tab_ids(state: &ClientShellState) -> Vec<String> {
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(_, id)| id.clone())
        .collect()
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })
}

#[test]
fn groups_render_headers_after_the_bucket_and_the_focused_group_never_folds() {
    let mut state = grouped_state();
    let frame = state.compose(106, 24).expect("composed frame");
    let headers = state
        .hits
        .sidebar_groups
        .iter()
        .map(|(rect, id)| (row_text(&frame, *rect), id.clone()))
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), 2);
    assert!(
        headers[0].0.contains("▾ api") && headers[0].0.contains('1'),
        "{headers:?}"
    );
    assert_eq!(headers[0].1, "ws_2");
    assert!(headers[1].0.contains("▾ infra"), "{headers:?}");
    let body = state.hits.agent_body;
    let rows = (0..body.height)
        .map(|offset| {
            row_text(
                &frame,
                ratatui::layout::Rect::new(body.x, body.y + offset, body.width, 1),
            )
            .trim()
            .to_string()
        })
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>();
    assert!(
        rows[0].contains("reviewer") && rows[1].contains("planner"),
        "{rows:?}"
    );
    assert!(
        rows[2].contains("api") && rows[3].contains("notes"),
        "{rows:?}"
    );
    assert!(
        rows[4].contains("infra") && rows[5].contains("deploy"),
        "{rows:?}"
    );

    state.collapsed_groups.insert(group_key("ws_2"));
    state.collapsed_groups.insert(group_key("ws_3"));
    state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2"]);

    let mut snapshot = three_space_snapshot();
    snapshot.focused_tab_id = Some("tab_4".into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_4";
    }
    state.set_snapshot(Box::new(snapshot));
    state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2", "tab_4"]);
}

#[test]
fn header_click_toggles_one_group_and_the_toolbar_folds_or_unfolds_all() {
    let mut state = grouped_state();
    let (header, _) = state.hits.sidebar_groups[0];
    let outcome = state.handle_raw_events(vec![
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            header.x + 3,
            header.y,
        ),
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            header.x + 3,
            header.y,
        ),
    ]);
    assert!(
        endpoint_methods(&outcome).is_empty(),
        "a header click folds, it does not focus the space"
    );
    assert!(state.collapsed_groups.contains(&group_key("ws_2")));
    state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2", "tab_4"]);

    // One group open, one folded: the toggle reads "fold all" and folds the rest.
    let toggle = state.hits.group_toggle_all;
    let frame = state.compose(106, 24).expect("composed frame");
    assert_eq!(row_text(&frame, toggle), FOLD_ALL_LABEL);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        toggle.x,
        toggle.y,
    )]);
    let frame = state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2"]);
    // Everything folded: the same button now reads "expand all" and expands.
    let toggle = state.hits.group_toggle_all;
    assert_eq!(row_text(&frame, toggle), UNFOLD_ALL_LABEL);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        toggle.x,
        toggle.y,
    )]);
    state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2", "tab_3", "tab_4"]);
    assert!(state.collapsed_groups.is_empty());
}

#[test]
fn header_menu_renames_ungroups_and_closes_a_group_without_touching_worktree_siblings() {
    let mut state = grouped_state();
    let (header, _) = state.hits.sidebar_groups[1];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Right),
        header.x + 3,
        header.y,
    )]);
    let items = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            assert!(matches!(
                menu.target,
                ClientContextMenuTarget::Group { ref workspace_id } if workspace_id == "ws_3"
            ));
            menu.items()
        }
        _ => panic!("group context menu"),
    };
    let actions = items.iter().map(|item| item.action).collect::<Vec<_>>();
    assert_eq!(
        actions,
        [
            ClientContextMenuAction::Rename,
            ClientContextMenuAction::Ungroup,
            ClientContextMenuAction::CloseGroup
        ]
    );

    state.compose(106, 24).expect("menu");
    let row = state.hits.context_menu_rows[1].0;
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        row.x + 1,
        row.y,
    )]);
    let moves = endpoint_methods(&outcome)
        .into_iter()
        .filter_map(|method| match method {
            crate::api::schema::Method::PaneMove(params) => Some(params.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].pane_id, "pane_4");
    assert!(matches!(
        &moves[0].destination,
        crate::api::schema::PaneMoveDestination::NewTab { workspace_id: Some(ws), label: Some(label) }
            if ws == "ws_1" && label == "deploy"
    ));

    state.compose(106, 24).expect("composed frame");
    let (header, _) = state.hits.sidebar_groups[0];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Right),
        header.x + 3,
        header.y,
    )]);
    state.compose(106, 24).expect("menu");
    let row = state.hits.context_menu_rows[2].0;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        row.x + 1,
        row.y,
    )]);
    // confirm_close is on by default: a "Close group?" prompt, then Enter closes
    // the space alone (never its worktree siblings).
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(ClientConfirmCloseOverlay {
            ref title, close_group: false, ..
        })) if title == "Close group?"
    ));
    let outcome = state.handle_input_bytes(b"\r");
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::WorkspaceClose(params)
            if params.workspace_id == "ws_2" && !params.close_group
    )));
}

#[test]
fn move_tab_to_group_prompt_joins_an_existing_group_or_creates_one() {
    use crate::input::{KeybindAction, KeybindMatch};
    let mut state = grouped_state();
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::MoveTabToGroup),
        &mut outcome,
    );
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::MoveTabToGroup { ref pane_id, .. },
            ..
        })) if pane_id == "pane_1"
    ));
    let outcome = state.handle_input_bytes(b"API\r");
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::PaneMove(params)
            if params.pane_id == "pane_1" && params.focus && matches!(
                &params.destination,
                crate::api::schema::PaneMoveDestination::NewTab { workspace_id: Some(ws), .. } if ws == "ws_2"
            )
    )));

    state.compose(106, 24).expect("composed frame");
    let plus = state.hits.group_new;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        plus.x,
        plus.y,
    )]);
    let outcome = state.handle_input_bytes(b"parked\r");
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::PaneMove(params)
            if matches!(
                &params.destination,
                crate::api::schema::PaneMoveDestination::NewWorkspace { label: Some(label), tab_label: Some(tab_label) }
                    if label == "parked" && tab_label == "reviewer"
            )
    )));
}

#[test]
fn header_drag_reorders_groups_and_tab_drag_moves_within_or_across_groups() {
    let mut state = grouped_state();
    let (api, _) = state.hits.sidebar_groups[0];
    let (infra, _) = state.hits.sidebar_groups[1];
    let outcome = state.handle_raw_events(vec![
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            infra.x + 3,
            infra.y,
        ),
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            infra.x + 3,
            api.y.saturating_sub(1),
        ),
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            infra.x + 3,
            api.y.saturating_sub(1),
        ),
    ]);
    assert!(
        endpoint_methods(&outcome).iter().any(|method| matches!(
            method,
            crate::api::schema::Method::WorkspaceMove(params)
                if params.workspace_id == "ws_3" && params.insert_index == 1
        )),
        "{:?}",
        endpoint_methods(&outcome)
    );

    state.compose(106, 24).expect("composed frame");
    let (reviewer, _) = state.hits.sidebar_tabs[0];
    let (planner, _) = state.hits.sidebar_tabs[1];
    let outcome = state.handle_raw_events(vec![
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            reviewer.x + 3,
            reviewer.y,
        ),
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            reviewer.x + 3,
            planner.y,
        ),
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            reviewer.x + 3,
            planner.y,
        ),
    ]);
    assert!(
        endpoint_methods(&outcome).iter().any(|method| matches!(
            method,
            crate::api::schema::Method::TabMove(params)
                if params.tab_id == "tab_1" && params.insert_index == 2
        )),
        "{:?}",
        endpoint_methods(&outcome)
    );

    state.compose(106, 24).expect("composed frame");
    let (planner, _) = state.hits.sidebar_tabs[1];
    let (api, _) = state.hits.sidebar_groups[0];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        planner.x + 3,
        planner.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        planner.x + 3,
        api.y,
    )]);
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        planner.x + 3,
        api.y,
    )]);
    assert!(
        endpoint_methods(&outcome).iter().any(|method| matches!(
            method,
            crate::api::schema::Method::PaneMove(params)
                if params.pane_id == "pane_2" && matches!(
                    &params.destination,
                    crate::api::schema::PaneMoveDestination::NewTab { workspace_id: Some(ws), label: Some(label) }
                        if ws == "ws_2" && label == "planner"
                )
        )),
        "{:?}",
        endpoint_methods(&outcome)
    );
}

#[test]
fn toggle_groups_folded_binding_mirrors_the_toolbar_toggle() {
    use crate::input::{KeybindAction, KeybindMatch};
    let mut state = grouped_state();
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ToggleGroupsFolded),
        &mut outcome,
    );
    assert!(outcome.repaint && outcome.actions.is_empty());
    state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2"]);
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ToggleGroupsFolded),
        &mut outcome,
    );
    state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2", "tab_3", "tab_4"]);
    let bound: Config = toml::from_str("[keys]\ntoggle_groups_folded = \"alt+e\"").unwrap();
    assert!(!bound
        .live_keybinds_with_diagnostics()
        .map(|(keybinds, _)| keybinds.keybinds.toggle_groups_folded.bindings.is_empty())
        .unwrap_or(true));
}

fn notices(outcome: &ClientShellInput, state: &ClientShellState) -> bool {
    let _ = outcome;
    state.visible_endpoint_notice.is_some()
}

#[test]
fn group_moves_refuse_multi_pane_tabs_and_the_buckets_last_tab() {
    // "notes" (tab_3, group api) gets a second pane; the bucket keeps one tab.
    let mut snapshot = three_space_snapshot();
    snapshot.panes.push(pane("pane_3b", "ws_2", "tab_3"));
    snapshot.tabs.retain(|tab| tab.tab_id != "tab_2");
    snapshot.panes.retain(|pane| pane.pane_id != "pane_2");
    let mut config = tabs_config();
    config.keys.move_tab_to_group = crate::config::BindingConfig::one("alt+g");
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("composed frame");

    // Dragging the split tab onto "infra" is refused with a notice, no pane.move.
    let (notes, _) = state.hits.sidebar_tabs[1];
    let (infra, _) = state.hits.sidebar_groups[1];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        notes.x + 3,
        notes.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        notes.x + 3,
        infra.y,
    )]);
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        notes.x + 3,
        infra.y,
    )]);
    assert!(!endpoint_methods(&outcome)
        .iter()
        .any(|method| matches!(method, crate::api::schema::Method::PaneMove(_))));
    assert!(notices(&outcome, &state), "a refusal must be visible");

    // Ungrouping "api" is all-or-nothing: the split member blocks it.
    state.visible_endpoint_notice = None;
    state.compose(106, 24).expect("composed frame");
    let (api, _) = state.hits.sidebar_groups[0];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Right),
        api.x + 3,
        api.y,
    )]);
    state.compose(106, 24).expect("menu");
    let row = state.hits.context_menu_rows[1].0;
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        row.x + 1,
        row.y,
    )]);
    assert!(endpoint_methods(&outcome).is_empty());
    assert!(notices(&outcome, &state));

    // The bucket's only tab ("reviewer") cannot leave via the prompt either.
    state.visible_endpoint_notice = None;
    let mut binding = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::MoveTabToGroup),
        &mut binding,
    );
    let outcome = state.handle_input_bytes(b"infra\r");
    assert!(endpoint_methods(&outcome).is_empty());
    assert!(notices(&outcome, &state));
}

#[test]
fn jittered_press_on_a_tab_row_still_focuses_it() {
    let mut state = grouped_state();
    let (planner, _) = state.hits.sidebar_tabs[1];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        planner.x + 3,
        planner.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        planner.x + 4,
        planner.y,
    )]);
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        planner.x + 4,
        planner.y,
    )]);
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_2"
    )));
}

#[test]
fn cross_group_drop_indicator_sits_on_the_target_groups_append_row() {
    let mut state = grouped_state();
    let (reviewer, _) = state.hits.sidebar_tabs[0];
    let (notes, _) = state.hits.sidebar_tabs[2];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        reviewer.x + 3,
        reviewer.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        reviewer.x + 3,
        notes.y,
    )]);
    match &state.chrome_drag {
        Some(ClientChromeDrag::SidebarTab {
            target: Some(target),
            ..
        }) => {
            assert_eq!(target.workspace_id, "ws_2");
            assert_eq!(target.insert_index, 1, "append to api");
            assert_eq!(target.row, notes.bottom(), "indicator below api's last tab");
        }
        other => panic!("expected a sidebar tab drag, got {:?}", other.is_some()),
    }
}

#[test]
fn focused_groups_header_click_is_a_no_op_and_the_toggle_ignores_it() {
    let mut snapshot = three_space_snapshot();
    snapshot.focused_tab_id = Some("tab_3".into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_3";
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("composed frame");
    let (api, _) = state.hits.sidebar_groups[0];
    state.handle_raw_events(vec![
        mouse(MouseEventKind::Down(MouseButton::Left), api.x + 3, api.y),
        mouse(MouseEventKind::Up(MouseButton::Left), api.x + 3, api.y),
    ]);
    assert!(
        state.collapsed_groups.is_empty(),
        "focused group never folds"
    );

    // Fold all: only infra folds, and the toggle then reads "expand all".
    let toggle = state.hits.group_toggle_all;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        toggle.x,
        toggle.y,
    )]);
    let frame = state.compose(106, 24).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2", "tab_3"]);
    assert_eq!(
        row_text(&frame, state.hits.group_toggle_all),
        UNFOLD_ALL_LABEL
    );
}

#[test]
fn toolbar_toggle_is_absent_when_only_the_focused_group_could_fold() {
    let mut snapshot = two_space_snapshot();
    snapshot.focused_tab_id = Some("tab_3".into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_3";
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    assert_eq!(
        state.hits.group_toggle_all,
        ratatui::layout::Rect::default()
    );
}

#[test]
fn move_prompt_treats_current_group_as_no_op_bucket_name_as_ungroup_and_skips_worktrees() {
    let mut snapshot = three_space_snapshot();
    // A linked worktree space labelled like a group name must never be a target.
    let mut worktree = snapshot.workspaces[2].clone();
    worktree.workspace_id = "ws_wt".into();
    worktree.label = "Api".into();
    worktree.worktree = Some(ClientShellWorktree {
        key: "/repo".into(),
        label: "api".into(),
        is_linked_worktree: true,
    });
    snapshot.workspaces.push(worktree);
    snapshot.focused_tab_id = Some("tab_3".into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_3";
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("composed frame");

    // "notes" already lives in api: naming its own group does nothing.
    let mut binding = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::MoveTabToGroup),
        &mut binding,
    );
    let outcome = state.handle_input_bytes(b"api\r");
    assert!(endpoint_methods(&outcome).is_empty());

    // Naming the bucket moves the tab back to the bucket.
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::MoveTabToGroup),
        &mut binding,
    );
    let outcome = state.handle_input_bytes(b"client-shell\r");
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::PaneMove(params) if matches!(
            &params.destination,
            crate::api::schema::PaneMoveDestination::NewTab { workspace_id: Some(ws), .. } if ws == "ws_1"
        )
    )));

    // "API" matches the group "api" case-insensitively, never the worktree "Api".
    let mut snapshot = three_space_snapshot();
    let mut worktree = snapshot.workspaces[2].clone();
    worktree.workspace_id = "ws_wt".into();
    worktree.label = "API".into();
    worktree.worktree = Some(ClientShellWorktree {
        key: "/repo".into(),
        label: "api".into(),
        is_linked_worktree: true,
    });
    snapshot.workspaces.push(worktree);
    state.set_snapshot(Box::new(snapshot));
    state.compose(106, 24).expect("composed frame");
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::MoveTabToGroup),
        &mut binding,
    );
    let outcome = state.handle_input_bytes(b"API\r");
    assert!(endpoint_methods(&outcome).iter().any(|method| matches!(
        method,
        crate::api::schema::Method::PaneMove(params) if matches!(
            &params.destination,
            crate::api::schema::PaneMoveDestination::NewTab { workspace_id: Some(ws), .. } if ws == "ws_2"
        )
    )));
}

#[test]
fn group_keys_do_nothing_outside_the_tabs_layout() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(three_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("composed frame");
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::MoveTabToGroup),
        &mut outcome,
    );
    assert!(state.overlay.is_none());
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::ToggleGroupsFolded),
        &mut outcome,
    );
    assert!(state.collapsed_groups.is_empty());
}

/// fg of every cell of `needle` inside `area`.
fn text_fgs(frame: &FrameData, area: ratatui::layout::Rect, needle: &str) -> Vec<u32> {
    let (x, y) = cell_symbol_position(frame, area, needle);
    (x..x + needle.chars().count() as u16)
        .map(|x| frame.cells[(y * frame.width + x) as usize].fg)
        .collect()
}

fn colored_tabs_state(config: &Config) -> (ClientShellState, FrameData) {
    use crate::api::schema::TabColor;
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![
        agent("pane_1", "tab_1", AgentStatus::Working),
        agent("pane_2", "tab_2", AgentStatus::Idle),
    ];
    snapshot.tabs[0].color = Some(TabColor::Purple);
    snapshot.tabs[1].color = Some(TabColor::Red);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    (state, frame)
}

#[test]
fn tab_color_tints_only_the_label_on_focused_and_unfocused_rows() {
    use crate::protocol::color_to_u32;
    use ratatui::style::{Color, Modifier};
    let (state, frame) = colored_tabs_state(&tabs_config());
    let palette = state.config.palette.clone();
    let rows = state.hits.sidebar_tabs.clone();
    let cell = |x: u16, y: u16| &frame.cells[(y * frame.width + x) as usize];

    // Focused row (purple -> mauve): label tinted, still bold on the selected background.
    let focused = rows[0].0;
    let mauve = color_to_u32(palette.mauve);
    assert!(text_fgs(&frame, focused, "reviewer")
        .iter()
        .all(|fg| *fg == mauve));
    let (x, y) = cell_symbol_position(&frame, focused, "reviewer");
    assert!(cell(x, y).modifier & Modifier::BOLD.bits() != 0);
    assert_eq!(cell(x, y).bg, color_to_u32(palette.active_row_bg));
    // The status icon and the brand-colored glyph keep their own colors.
    assert_eq!(
        cell(focused.x + 1, focused.y).fg,
        color_to_u32(status_color(AgentStatus::Working, &palette))
    );
    let glyphs = glyph_cells(&state, &frame);
    assert_eq!(glyphs[0].1, color_to_u32(Color::Rgb(0xD9, 0x77, 0x57)));

    // Unfocused row (red): label tinted, glyph stays monochrome.
    let unfocused = rows[1].0;
    let red = color_to_u32(palette.red);
    assert!(text_fgs(&frame, unfocused, "planner")
        .iter()
        .all(|fg| *fg == red));
    let (x, y) = cell_symbol_position(&frame, unfocused, "planner");
    assert!(cell(x, y).modifier & Modifier::BOLD.bits() == 0);
    assert_eq!(
        cell(unfocused.x + 1, unfocused.y).fg,
        color_to_u32(status_color(AgentStatus::Idle, &palette))
    );
    assert_eq!(glyphs[1].1, color_to_u32(palette.overlay0));

    // An uncolored tab keeps the default label color.
    let plain = rows[2].0;
    let subtext = color_to_u32(palette.subtext0);
    assert!(text_fgs(&frame, plain, "notes")
        .iter()
        .all(|fg| *fg == subtext));
}

#[test]
fn tab_color_maps_every_name_to_a_theme_slot_and_ignores_unknown() {
    use crate::api::schema::TabColor;
    let palette = ClientShellConfig::from_config(&tabs_config()).palette;
    let fg = |color| super::super::tab_color::tab_color_fg(color, &palette);
    assert_eq!(fg(TabColor::Red), Some(palette.red));
    assert_eq!(fg(TabColor::Orange), Some(palette.peach));
    assert_eq!(fg(TabColor::Yellow), Some(palette.yellow));
    assert_eq!(fg(TabColor::Green), Some(palette.green));
    assert_eq!(fg(TabColor::Cyan), Some(palette.teal));
    assert_eq!(fg(TabColor::Blue), Some(palette.blue));
    assert_eq!(fg(TabColor::Purple), Some(palette.mauve));
    assert_eq!(fg(TabColor::Unknown), None);

    // A newer server's color name decodes as Unknown and draws uncolored;
    // an older server's snapshot without the key decodes as no color.
    let mut value = serde_json::to_value(two_space_snapshot()).unwrap();
    value["tabs"][1]["color"] = "magenta".into();
    value["tabs"][2].as_object_mut().unwrap().remove("color");
    let snapshot: ClientShellSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(snapshot.tabs[1].color, Some(TabColor::Unknown));
    assert_eq!(snapshot.tabs[2].color, None);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    let row = state.hits.sidebar_tabs[1].0;
    let subtext = crate::protocol::color_to_u32(state.config.palette.subtext0);
    assert!(text_fgs(&frame, row, "planner")
        .iter()
        .all(|fg| *fg == subtext));
}

#[test]
fn spaces_layout_tints_unfocused_tab_bar_labels_only() {
    use crate::protocol::color_to_u32;
    let (state, frame) = colored_tabs_state(&Config::default());
    let palette = &state.config.palette;
    let rect = |tab_id: &str| {
        state
            .hits
            .tabs
            .iter()
            .find(|(_, id)| id == tab_id)
            .map(|(rect, _)| *rect)
            .expect("tab bar hit")
    };
    let red = color_to_u32(palette.red);
    assert!(text_fgs(&frame, rect("tab_2"), "planner")
        .iter()
        .all(|fg| *fg == red));
    let contrast = color_to_u32(panel_contrast_fg(palette));
    assert!(text_fgs(&frame, rect("tab_1"), "reviewer")
        .iter()
        .all(|fg| *fg == contrast));
}

fn open_color_picker(state: &mut ClientShellState, row: usize) -> usize {
    let items = tab_menu_items(state, row);
    let color = items
        .iter()
        .position(|item| item.action == ClientContextMenuAction::Color)
        .expect("color item");
    assert_eq!(color, items.len() - 1, "Color is the last tab menu item");
    assert_eq!(items[color].label, "Color");
    state.compose(106, 20).expect("tab context menu");
    let menu_row = state.hits.context_menu_rows[color].0;
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        menu_row.x + 1,
        menu_row.y,
    )]);
    assert!(
        endpoint_methods(&outcome).is_empty(),
        "opening the picker sends nothing (not even a tab focus)"
    );
    color
}

fn key(code: crossterm::event::KeyCode) -> RawInputEvent {
    RawInputEvent::Key(crate::input::TerminalKey::new(code, KeyModifiers::empty()))
}

fn picked_colors(
    outcome: &ClientShellInput,
) -> Vec<(String, Option<crate::api::schema::TabColor>)> {
    endpoint_methods(outcome)
        .into_iter()
        .filter_map(|method| match method {
            crate::api::schema::Method::TabSetColor(params) => {
                Some((params.tab_id.clone(), params.color))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn color_menu_item_opens_a_swatch_row_marking_the_current_color() {
    use crate::api::schema::TabColor;
    use crate::protocol::color_to_u32;
    let (mut state, _) = colored_tabs_state(&tabs_config());
    open_color_picker(&mut state, 0);
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("picker overlay");
    };
    assert!(matches!(
        &menu.target,
        ClientContextMenuTarget::TabColor { tab_id, current: Some(TabColor::Purple) }
            if tab_id == "tab_1"
    ));
    assert_eq!(menu.highlighted, 7, "the current color starts highlighted");
    let labels = menu
        .items()
        .iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        ["none", "red", "orange", "yellow", "green", "cyan", "blue", "purple"]
    );

    let frame = state.compose(106, 20).expect("picker frame");
    let swatches = state.hits.context_menu_rows.clone();
    assert_eq!(swatches.len(), 8);
    for (index, (rect, hit)) in swatches.iter().enumerate() {
        assert_eq!(*hit, index);
        assert_eq!((rect.width, rect.height), (3, 1));
        assert_eq!(rect.y, swatches[0].0.y, "one row");
        assert_eq!(rect.x, swatches[0].0.x + 3 * index as u16, "side by side");
    }
    let text = |rect: ratatui::layout::Rect| row_text(&frame, rect);
    assert_eq!(text(swatches[0].0), " \u{2205} ");
    assert_eq!(text(swatches[1].0), " \u{25A0} ");
    assert_eq!(
        text(swatches[7].0),
        "[\u{25A0}]",
        "current color is bracketed"
    );
    let glyph_fg = |index: usize| {
        let rect = swatches[index].0;
        frame.cells[(rect.y * frame.width + rect.x + 1) as usize].fg
    };
    let palette = &state.config.palette;
    assert_eq!(glyph_fg(0), color_to_u32(palette.overlay0));
    assert_eq!(glyph_fg(1), color_to_u32(palette.red));
    assert_eq!(glyph_fg(2), color_to_u32(palette.peach));
    assert_eq!(glyph_fg(5), color_to_u32(palette.teal));
    assert_eq!(glyph_fg(7), color_to_u32(palette.mauve));
    // The highlighted swatch wears the menu's highlight background.
    let highlight_bg = |index: usize| {
        let rect = swatches[index].0;
        frame.cells[(rect.y * frame.width + rect.x) as usize].bg
    };
    assert_eq!(highlight_bg(7), color_to_u32(palette.accent));
    assert_eq!(highlight_bg(3), color_to_u32(palette.panel_bg));
}

#[test]
fn color_picker_keys_move_along_the_row_and_enter_picks() {
    use crate::api::schema::TabColor;
    use crossterm::event::KeyCode;
    let (mut state, _) = colored_tabs_state(&tabs_config());
    open_color_picker(&mut state, 0);
    let highlighted = |state: &ClientShellState| match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.highlighted,
        _ => panic!("picker overlay"),
    };
    state.handle_raw_events(vec![key(KeyCode::Right)]);
    assert_eq!(highlighted(&state), 7, "stops at the last swatch");
    state.handle_raw_events(vec![key(KeyCode::Left), key(KeyCode::Char('h'))]);
    assert_eq!(highlighted(&state), 5);
    state.handle_raw_events(vec![key(KeyCode::Char('l'))]);
    assert_eq!(highlighted(&state), 6);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert_eq!(
        picked_colors(&outcome),
        [("tab_1".to_string(), Some(TabColor::Blue))]
    );
    assert!(state.overlay.is_none());

    // Esc closes without a request.
    open_color_picker(&mut state, 1);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(picked_colors(&outcome).is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn clicking_a_swatch_sends_tab_set_color_at_once() {
    use crate::api::schema::TabColor;
    let (mut state, _) = colored_tabs_state(&tabs_config());
    open_color_picker(&mut state, 1);
    state.compose(106, 20).expect("picker frame");
    let swatch = state.hits.context_menu_rows[4].0;
    // Hovering highlights; the click on a swatch's edge cell still picks it.
    state.handle_raw_events(vec![mouse(MouseEventKind::Moved, swatch.x, swatch.y)]);
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::ContextMenu(menu)) if menu.highlighted == 4
    ));
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        swatch.right() - 1,
        swatch.y,
    )]);
    assert_eq!(
        picked_colors(&outcome),
        [("tab_2".to_string(), Some(TabColor::Green))]
    );
    assert!(state.overlay.is_none());

    // The none swatch clears.
    open_color_picker(&mut state, 1);
    state.compose(106, 20).expect("picker frame");
    let none = state.hits.context_menu_rows[0].0;
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        none.x + 1,
        none.y,
    )]);
    assert_eq!(picked_colors(&outcome), [("tab_2".to_string(), None)]);
}

#[test]
fn cycle_tab_color_binding_steps_the_focused_tab_and_wraps() {
    use crate::api::schema::TabColor;
    use crate::input::{KeybindAction, KeybindMatch};
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    let mut snapshot = two_space_snapshot();
    let mut expected = Vec::new();
    let mut current = None;
    for _ in 0..8 {
        snapshot.tabs[0].color = current;
        state.set_snapshot(Box::new(snapshot.clone()));
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            KeybindMatch::Action(KeybindAction::CycleTabColor),
            &mut outcome,
        );
        let picked = picked_colors(&outcome);
        assert_eq!(picked.len(), 1, "{picked:?}");
        assert_eq!(picked[0].0, "tab_1", "targets the focused tab");
        expected.push(picked[0].1);
        current = picked[0].1;
    }
    assert_eq!(
        expected,
        [
            Some(TabColor::Red),
            Some(TabColor::Orange),
            Some(TabColor::Yellow),
            Some(TabColor::Green),
            Some(TabColor::Cyan),
            Some(TabColor::Blue),
            Some(TabColor::Purple),
            None,
        ]
    );
    let bound: Config = toml::from_str("[keys]\ncycle_tab_color = \"alt+c\"").unwrap();
    assert!(!bound
        .live_keybinds_with_diagnostics()
        .map(|(keybinds, _)| keybinds.keybinds.cycle_tab_color.bindings.is_empty())
        .unwrap_or(true));
    assert!(!Config::default().keys.cycle_tab_color.has_values());
}

/// Fork smoke tests: FORK.md section 10 lists them by name and the sync gate
/// runs them with `-E 'test(fork_smoke)'`.
mod fork_smoke {
    use super::*;
    use crossterm::event::KeyCode;

    fn tabs_state_with_agent(status: AgentStatus) -> ClientShellState {
        let mut snapshot = two_space_snapshot();
        snapshot.agents = vec![agent("pane_1", "tab_1", status)];
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(wide_surface());
        state.compose(106, 20).expect("composed frame");
        state
    }

    fn typed_input() -> Vec<RawInputEvent> {
        vec![
            RawInputEvent::Key(crate::input::TerminalKey::new(
                KeyCode::Char('l'),
                KeyModifiers::empty(),
            )),
            RawInputEvent::Key(crate::input::TerminalKey::new(
                KeyCode::Enter,
                KeyModifiers::empty(),
            )),
            RawInputEvent::Key(crate::input::TerminalKey::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )),
            RawInputEvent::Text(crate::input::TextCommit::new("ls")),
            RawInputEvent::Paste("rm -rf /".into()),
        ]
    }

    fn pane_input_requests(outcome: &ClientShellInput) -> usize {
        outcome
            .requests
            .iter()
            .filter(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. }))
            .count()
    }

    /// The tabs-layout input lock guards specific client entry points. An
    /// upstream input route that bypasses them would type into the bare shell
    /// behind the suspended card without any merge conflict.
    #[test]
    fn locked_suspended_pane_emits_no_pane_input() {
        // Control: the same keys on a live agent pane do reach the pane, so
        // the locked assertion below cannot pass because input moved to a
        // different message.
        let mut live = tabs_state_with_agent(AgentStatus::Working);
        let outcome = live.handle_raw_events(typed_input());
        assert!(
            pane_input_requests(&outcome) > 0,
            "typed input must reach a live pane"
        );

        let mut parked = tabs_state_with_agent(AgentStatus::Suspended);
        assert!(parked.suspended_pane_locked("pane_1"));
        let outcome = parked.handle_raw_events(typed_input());
        assert_eq!(
            pane_input_requests(&outcome),
            0,
            "no typed input may reach a suspended pane"
        );
        let pane = parked.hits.panes[0].inner_rect;
        let outcome = parked.handle_raw_events(
            [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::ScrollUp,
            ]
            .into_iter()
            .map(|kind| {
                RawInputEvent::Mouse(MouseEvent {
                    kind,
                    column: pane.x + 2,
                    row: pane.y + 1,
                    modifiers: KeyModifiers::empty(),
                })
            })
            .collect(),
        );
        assert_eq!(
            pane_input_requests(&outcome),
            0,
            "no mouse input may reach a suspended pane"
        );
    }

    /// A colored tab's label reaches the renderer in its mapped theme color.
    /// An upstream change to how sidebar rows are styled would silently drop
    /// the tint without any merge conflict.
    #[test]
    fn colored_tab_label_reaches_the_renderer_in_its_color() {
        let mut snapshot = two_space_snapshot();
        snapshot.tabs[2].color = Some(crate::api::schema::TabColor::Green);
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(wide_surface());
        let frame = state.compose(106, 20).expect("composed frame");
        let row = state.hits.sidebar_tabs[2].0;
        let green = crate::protocol::color_to_u32(state.config.palette.green);
        let fgs = text_fgs(&frame, row, "notes");
        assert!(fgs.iter().all(|fg| *fg == green), "{fgs:?}");
    }
}
