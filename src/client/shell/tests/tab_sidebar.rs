use super::super::tab_sidebar::{FOLD_ALL_LABEL, UNFOLD_ALL_LABEL};
use super::*;
use crate::config::{Config, SidebarLayoutConfig};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

/// A tab row's status icon column (the label starts at x=7).
const STATUS_ICON_X: u16 = 5;

/// The tabs layout without the Active agents block: these tests frame the
/// list rows, and the block (`tests/sidebar_active.rs`) would move them.
fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config.ui.sidebar_active_agents = false;
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
        important: false,
        remind_every: None,
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
        subagents: 0,
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

/// Each tab row's text, trimmed at the end, in row order.
fn tab_rows(state: &ClientShellState, frame: &FrameData) -> Vec<String> {
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| row_text(frame, *rect).trim_end().to_string())
        .collect()
}

fn hover_tab(state: &mut ClientShellState, tab_id: &str) -> FrameData {
    state.sidebar_hover = Some(super::super::sidebar_model::SidebarHover::Tab(
        tab_id.into(),
    ));
    state.compose(106, 20).expect("composed frame")
}

#[test]
fn harness_glyph_shows_only_on_the_focused_row() {
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
    // Nothing hovered: only the focused tab (claude) shows its glyph.
    let rows = tab_rows(&state, &frame);
    assert!(rows[0].ends_with('\u{29C6}'), "claude: {rows:?}");
    assert!(rows[0].contains("reviewer"), "{rows:?}");
    for row in &rows[1..] {
        assert!(row.ends_with(|c: char| c.is_alphanumeric()), "{rows:?}");
    }

    // Hovering another row (or its Active agents entry) leaves the glyph on
    // the focused row; the hovered row shows none.
    let glyphs = [
        (1, "tab_2", '\u{29C7}'), // codex
        (2, "tab_3", '\u{237E}'), // another agent
        (3, "tab_4", '\u{29C5}'), // a plain shell
    ];
    for (_, tab_id, _) in glyphs {
        for hover in [
            super::super::sidebar_model::SidebarHover::Tab(tab_id.into()),
            super::super::sidebar_model::SidebarHover::ActiveEntry(tab_id.into()),
        ] {
            state.sidebar_hover = Some(hover);
            let frame = state.compose(106, 20).expect("composed frame");
            let rows = tab_rows(&state, &frame);
            assert!(
                rows[0].ends_with('\u{29C6}'),
                "the focused row keeps the glyph: {rows:?}"
            );
            for row in &rows[1..] {
                assert!(row.ends_with(|c: char| c.is_alphanumeric()), "{rows:?}");
            }
        }
    }
    let frame = hover_tab(&mut state, "tab_gone");
    assert!(tab_rows(&state, &frame)[0].ends_with('\u{29C6}'));

    // Focusing another tab moves the glyph there, the rest none.
    state.sidebar_hover = None;
    for (index, tab_id, glyph) in glyphs {
        let mut focused = snapshot.clone();
        for tab in &mut focused.tabs {
            tab.focused = tab.tab_id == tab_id;
        }
        state.set_snapshot(Box::new(focused));
        let frame = state.compose(106, 20).expect("composed frame");
        let rows = tab_rows(&state, &frame);
        for (row_index, row) in rows.iter().enumerate() {
            assert_eq!(
                row.ends_with(glyph),
                row_index == index,
                "{tab_id}: {rows:?}"
            );
        }
        assert!(!rows[0].ends_with('\u{29C6}'), "{rows:?}");
    }

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

/// The glyph cell (symbol, fg, bg) of each tab row, in row order.
fn glyph_cells(state: &ClientShellState, frame: &FrameData) -> Vec<(String, u32, u32)> {
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| {
            // The harness glyph is the last mark, one cell in from the right.
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
    let (mut state, frame) = glyph_color_state(&tabs_config(), "tab_1");
    let palette = state.config.palette.clone();
    let orange = color_to_u32(Color::Rgb(0xD9, 0x77, 0x57));
    let cells = glyph_cells(&state, &frame);
    assert_eq!(cells[0].0, "\u{29C6}", "{cells:?}");
    assert_eq!(cells[0].1, orange, "focused claude glyph is orange");
    assert_eq!(
        cells[0].2,
        color_to_u32(palette.active_row_bg),
        "keeps the selected row background"
    );
    assert_eq!(
        cells[1].0, " ",
        "an unselected row shows no glyph: {cells:?}"
    );

    // Hovering another row leaves the glyph on the focused one.
    let frame = hover_tab(&mut state, "tab_2");
    let cells = glyph_cells(&state, &frame);
    assert_eq!(cells[0].0, "\u{29C6}", "{cells:?}");
    assert_eq!(cells[1].0, " ", "the hovered row shows no glyph: {cells:?}");

    // The hovered focused row's glyph wears the brand color on the band.
    let frame = hover_tab(&mut state, "tab_1");
    let cells = glyph_cells(&state, &frame);
    assert_eq!(cells[0].0, "\u{29C6}", "{cells:?}");
    assert_eq!(cells[0].1, orange);
    assert_eq!(cells[0].2, color_to_u32(palette.sidebar_hover_bg()));

    // A focused shell tab keeps the monochrome glyph.
    let (state, frame) = glyph_color_state(&tabs_config(), "tab_3");
    let cells = glyph_cells(&state, &frame);
    assert_eq!(cells[2].0, "\u{29C5}", "{cells:?}");
    assert_eq!(cells[2].1, color_to_u32(state.config.palette.overlay0));
    assert_eq!(cells[2].2, color_to_u32(state.config.palette.active_row_bg));
    assert_eq!(cells[0].0, " ", "{cells:?}");
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
    grouped_state_at(24)
}

/// `grouped_state` composed at `rows` (from `TALL_ROWS` a spacer row sits
/// before every group header).
fn grouped_state_at(rows: u16) -> ClientShellState {
    let mut config = tabs_config();
    config.keys.move_tab_to_group = crate::config::BindingConfig::one("alt+g");
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(three_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, rows).expect("composed frame");
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
    // The fold marker at x=1, the name at x=5 (x=3 holds a team's mark).
    assert!(
        headers[0].0.starts_with(" ▾   api") && headers[0].0.contains('1'),
        "{headers:?}"
    );
    assert_eq!(headers[0].1, "ws_2");
    assert!(headers[1].0.starts_with(" ▾   infra"), "{headers:?}");
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
    // Fold state changed outside the toggles: the list rows rebuild.
    state.sidebar_model.mark_dirty();
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
                ClientContextMenuTarget::Group { ref workspace_id, .. } if workspace_id == "ws_3"
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
            ClientContextMenuAction::CloseGroup,
            // Fork: the endpoint advertises every method (no list), so the
            // team item shows after upstream's (`tests/teams.rs`).
            ClientContextMenuAction::MakeTeam,
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
    // Tall enough for the spacer rows before the headers.
    let mut state = grouped_state_at(TALL_ROWS);
    let (api, _) = state.hits.sidebar_groups[0];
    assert!(
        state
            .hits
            .sidebar_tabs
            .iter()
            .all(|(rect, _)| rect.y != api.y - 1),
        "a spacer row sits above the header"
    );
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

    state.compose(106, TALL_ROWS).expect("composed frame");
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

    state.compose(106, TALL_ROWS).expect("composed frame");
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
    // Tall enough for the spacer rows: the append row is the spacer under
    // api's last tab.
    let mut state = grouped_state_at(TALL_ROWS);
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

    // Focused row (purple): label tinted, still bold on the selected background.
    let focused = rows[0].0;
    let mauve = color_to_u32(
        super::super::tab_color::tab_color_fg(crate::api::schema::TabColor::Purple, &palette)
            .unwrap(),
    );
    assert!(text_fgs(&frame, focused, "reviewer")
        .iter()
        .all(|fg| *fg == mauve));
    let (x, y) = cell_symbol_position(&frame, focused, "reviewer");
    assert!(cell(x, y).modifier & Modifier::BOLD.bits() != 0);
    assert_eq!(cell(x, y).bg, color_to_u32(palette.active_row_bg));
    // The status icon (x=5) and the brand-colored glyph keep their own colors.
    assert_eq!(
        cell(focused.x + 5, focused.y).fg,
        color_to_u32(status_color(AgentStatus::Working, &palette))
    );
    let glyphs = glyph_cells(&state, &frame);
    assert_eq!(glyphs[0].1, color_to_u32(Color::Rgb(0xD9, 0x77, 0x57)));

    // Unfocused row (red): label tinted, no glyph (not selected).
    let unfocused = rows[1].0;
    let red = color_to_u32(
        super::super::tab_color::tab_color_fg(crate::api::schema::TabColor::Red, &palette).unwrap(),
    );
    assert!(text_fgs(&frame, unfocused, "planner")
        .iter()
        .all(|fg| *fg == red));
    let (x, y) = cell_symbol_position(&frame, unfocused, "planner");
    assert!(cell(x, y).modifier & Modifier::BOLD.bits() == 0);
    assert_eq!(
        cell(unfocused.x + 5, unfocused.y).fg,
        color_to_u32(status_color(AgentStatus::Idle, &palette))
    );
    assert_eq!(glyphs[1].0, " ");

    // An uncolored tab keeps the default label color.
    let plain = rows[2].0;
    let subtext = color_to_u32(palette.subtext0);
    assert!(text_fgs(&frame, plain, "notes")
        .iter()
        .all(|fg| *fg == subtext));
}

#[test]
fn tab_color_maps_every_name_to_a_fixed_color_and_ignores_unknown() {
    use crate::api::schema::TabColor;
    let palette = ClientShellConfig::from_config(&tabs_config()).palette;
    let fg = |color| super::super::tab_color::tab_color_fg(color, &palette);
    // Fixed true colors, not theme slots: 16-color themes remap the ANSI
    // slots (Carbonfox's "yellow" is teal), so every color must be distinct
    // from the others and from the default label colors.
    let colors = TabColor::ALL.map(|color| fg(color).expect("mapped"));
    for (i, a) in colors.iter().enumerate() {
        assert!(matches!(a, ratatui::style::Color::Rgb(..)), "{a:?}");
        for b in &colors[i + 1..] {
            assert_ne!(a, b);
        }
        assert_ne!(*a, palette.text);
        assert_ne!(*a, palette.subtext0);
    }
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
    // Uncolored, the blocked tab's label takes the status emphasis (`text`).
    let text = crate::protocol::color_to_u32(state.config.palette.text);
    assert!(text_fgs(&frame, row, "planner")
        .iter()
        .all(|fg| *fg == text));
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
    let red = color_to_u32(
        super::super::tab_color::tab_color_fg(crate::api::schema::TabColor::Red, palette).unwrap(),
    );
    assert!(text_fgs(&frame, rect("tab_2"), "planner")
        .iter()
        .all(|fg| *fg == red));
    let contrast = color_to_u32(panel_contrast_fg(palette));
    assert!(text_fgs(&frame, rect("tab_1"), "reviewer")
        .iter()
        .all(|fg| *fg == contrast));
}

/// Open the tab menu on sidebar row `row` and return the swatch row's index,
/// which must be the last item.
fn open_menu_with_swatches(state: &mut ClientShellState, row: usize) -> usize {
    let items = tab_menu_items(state, row);
    let color = items
        .iter()
        .position(|item| item.action == ClientContextMenuAction::Color)
        .expect("swatch row");
    assert_eq!(color, items.len() - 1, "the swatch row ends the tab menu");
    state.compose(106, 20).expect("tab context menu");
    color
}

fn menu_state(state: &ClientShellState) -> (usize, ClientTabMenuColor) {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { color, .. },
            highlighted,
            ..
        })) => (*highlighted, *color),
        _ => panic!("tab context menu"),
    }
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
fn tab_menu_ends_with_a_swatch_row_marking_the_current_color() {
    use crate::api::schema::TabColor;
    use crate::protocol::color_to_u32;
    use crossterm::event::KeyCode;
    let (mut state, _) = colored_tabs_state(&tabs_config());
    // Without an agent Close stays the third item (upstream's close_tab tests).
    let plain = tab_menu_items(&mut state, 2);
    assert_eq!(plain[2].action, ClientContextMenuAction::Close);
    // Fork: the tab's "Info pane" follows Close (no agent: no role item).
    assert_eq!(plain[3].action, ClientContextMenuAction::ToggleInfoPane);
    assert_eq!(plain[4].action, ClientContextMenuAction::Important);
    assert_eq!(plain[5].action, ClientContextMenuAction::RemindTop);
    assert_eq!(plain[6].action, ClientContextMenuAction::RemindBottom);
    assert_eq!(plain.len(), 8);

    // tab_2 is red, one of the offered colors.
    let row = open_menu_with_swatches(&mut state, 1);
    assert_eq!(
        menu_state(&state).1,
        ClientTabMenuColor {
            current: Some(TabColor::Red),
            cursor: 1
        }
    );
    let frame = state.compose(106, 20).expect("menu frame");
    let menu_row = state.hits.context_menu_rows[row].0;
    let swatches = state.hits.context_menu_swatches.clone();
    assert_eq!(swatches.len(), 5, "none + the four offered colors");
    assert!(menu_row.width >= 15, "the menu fits the row");
    for (index, (rect, hit)) in swatches.iter().enumerate() {
        assert_eq!(*hit, index);
        assert_eq!((rect.width, rect.height), (3, 1));
        assert_eq!(rect.y, menu_row.y, "inside the last menu row");
        assert_eq!(rect.x, menu_row.x + 3 * index as u16, "side by side");
    }
    let text = |rect: ratatui::layout::Rect| row_text(&frame, rect);
    assert_eq!(text(swatches[0].0), " \u{2205} ");
    assert_eq!(
        text(swatches[1].0),
        "[\u{25A0}]",
        "current color is bracketed"
    );
    assert_eq!(text(swatches[2].0), " \u{25A0} ");
    let cell = |rect: ratatui::layout::Rect, dx: u16| {
        frame.cells[(rect.y * frame.width + rect.x + dx) as usize].clone()
    };
    let palette = state.config.palette.clone();
    let tag = |color| super::super::tab_color::tab_color_fg(color, &palette).unwrap();
    let expected = [
        palette.overlay0,
        tag(crate::api::schema::TabColor::Red),
        tag(crate::api::schema::TabColor::Yellow),
        tag(crate::api::schema::TabColor::Green),
        tag(crate::api::schema::TabColor::Purple),
    ];
    for (index, color) in expected.into_iter().enumerate() {
        assert_eq!(cell(swatches[index].0, 1).fg, color_to_u32(color));
    }
    // No swatch is highlighted until the row is.
    assert!(swatches
        .iter()
        .all(|(rect, _)| cell(*rect, 0).bg == color_to_u32(palette.panel_bg)));

    state.handle_raw_events((0..12).map(|_| key(KeyCode::Down)).collect());
    assert_eq!(menu_state(&state).0, row);
    let frame = state.compose(106, 20).expect("menu frame");
    let bg = |index: usize| {
        let rect = state.hits.context_menu_swatches[index].0;
        frame.cells[(rect.y * frame.width + rect.x) as usize].bg
    };
    assert_eq!(
        bg(1),
        color_to_u32(palette.accent),
        "cursor on the current color"
    );
    assert_eq!(bg(3), color_to_u32(palette.panel_bg));

    // tab_1 is purple, the last offered color: the cursor starts there and
    // only that swatch is bracketed.
    open_menu_with_swatches(&mut state, 0);
    assert_eq!(menu_state(&state).1.cursor, 4);
    let frame = state.compose(106, 20).expect("menu frame");
    let bracketed = state
        .hits
        .context_menu_swatches
        .iter()
        .filter(|(rect, _)| row_text(&frame, *rect).starts_with('['))
        .map(|(_, index)| *index)
        .collect::<Vec<_>>();
    assert_eq!(bracketed, [4]);
}

#[test]
fn swatch_row_keys_move_along_the_row_and_enter_picks_without_focusing() {
    use crate::api::schema::{Method, TabColor};
    use crossterm::event::KeyCode;
    let (mut state, _) = colored_tabs_state(&tabs_config());
    let row = open_menu_with_swatches(&mut state, 0);
    // Left/right mean nothing on an ordinary row.
    state.handle_raw_events(vec![key(KeyCode::Right), key(KeyCode::Char('l'))]);
    assert_eq!(menu_state(&state), (0, menu_state(&state).1));
    assert_eq!(menu_state(&state).1.cursor, 4);

    state.handle_raw_events((0..12).map(|_| key(KeyCode::Down)).collect());
    assert_eq!(menu_state(&state).0, row, "the swatch row is one row");
    assert_eq!(
        menu_state(&state).1.cursor,
        4,
        "starts on the current color (purple)"
    );
    state.handle_raw_events((0..6).map(|_| key(KeyCode::Right)).collect());
    assert_eq!(menu_state(&state).1.cursor, 4, "stops at the last swatch");
    state.handle_raw_events(vec![key(KeyCode::Left), key(KeyCode::Char('h'))]);
    assert_eq!(menu_state(&state).1.cursor, 2);
    state.handle_raw_events(vec![key(KeyCode::Char('l'))]);
    assert_eq!(menu_state(&state).1.cursor, 3);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert_eq!(
        picked_colors(&outcome),
        [("tab_1".to_string(), Some(TabColor::Green))]
    );
    assert!(!endpoint_methods(&outcome)
        .iter()
        .any(|method| matches!(method, Method::TabFocus(_))));
    assert!(state.overlay.is_none());

    // Re-entering the row puts the cursor back on the current color.
    let row = open_menu_with_swatches(&mut state, 1);
    state.handle_raw_events((0..12).map(|_| key(KeyCode::Down)).collect());
    state.handle_raw_events(vec![key(KeyCode::Right)]);
    assert_eq!(menu_state(&state).1.cursor, 2);
    state.handle_raw_events(vec![key(KeyCode::Up), key(KeyCode::Down)]);
    assert_eq!(menu_state(&state), (row, menu_state(&state).1));
    assert_eq!(menu_state(&state).1.cursor, 1, "red again");

    // Esc closes without a request.
    let outcome = state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(picked_colors(&outcome).is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn clicking_a_swatch_sends_tab_set_color_and_closes_the_menu() {
    use crate::api::schema::{Method, TabColor};
    let (mut state, _) = colored_tabs_state(&tabs_config());
    let row = open_menu_with_swatches(&mut state, 1);
    let swatch = state.hits.context_menu_swatches[4].0;
    // Hovering highlights the row and moves the cursor; a click on a
    // swatch's edge cell still picks it.
    state.handle_raw_events(vec![mouse(MouseEventKind::Moved, swatch.x, swatch.y)]);
    assert_eq!(menu_state(&state).0, row);
    assert_eq!(menu_state(&state).1.cursor, 4);
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        swatch.right() - 1,
        swatch.y,
    )]);
    assert_eq!(
        picked_colors(&outcome),
        [("tab_2".to_string(), Some(TabColor::Purple))]
    );
    assert!(!endpoint_methods(&outcome)
        .iter()
        .any(|method| matches!(method, Method::TabFocus(_))));
    assert!(state.overlay.is_none());

    // The none swatch clears.
    open_menu_with_swatches(&mut state, 1);
    let none = state.hits.context_menu_swatches[0].0;
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
    for _ in 0..5 {
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
            Some(TabColor::Yellow),
            Some(TabColor::Green),
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

#[test]
fn working_tab_with_subagents_shows_the_subagent_icon() {
    use crate::config::StatusIndicatorStyle;
    let icon = super::super::tab_sidebar::TAB_SUBAGENTS_ICON;
    for style in [StatusIndicatorStyle::Dots, StatusIndicatorStyle::Symbols] {
        let mut snapshot = two_space_snapshot();
        snapshot.tabs[2].agent_status = AgentStatus::Working;
        let mut with_subagents = agent("pane_1", "tab_1", AgentStatus::Working);
        with_subagents.subagents = 2;
        let mut blocked = agent("pane_2", "tab_2", AgentStatus::Blocked);
        blocked.subagents = 3;
        snapshot.agents = vec![
            with_subagents,
            blocked,
            agent("pane_3", "tab_3", AgentStatus::Working),
        ];
        let mut config = tabs_config();
        config.ui.status_indicators = style;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        let frame = state.compose(106, 20).expect("composed frame");
        let icon_cell = |row: usize| {
            let (rect, _) = state.hits.sidebar_tabs[row];
            frame.cells[(rect.y * frame.width + rect.x + STATUS_ICON_X) as usize].clone()
        };
        assert_eq!(
            icon_cell(0).symbol,
            icon,
            "{style:?}: working with subagents"
        );
        assert_ne!(
            icon_cell(1).symbol,
            icon,
            "{style:?}: blocked keeps its icon"
        );
        assert_ne!(icon_cell(2).symbol, icon, "{style:?}: no subagents");
        assert_eq!(
            icon_cell(0).fg,
            icon_cell(2).fg,
            "{style:?}: drawn in the working color"
        );
    }
}

#[test]
fn subagents_show_on_idle_and_finished_agents_in_their_status_color() {
    use crate::config::StatusIndicatorStyle;
    use crate::protocol::color_to_u32;
    let icon = super::super::tab_sidebar::TAB_SUBAGENTS_ICON;
    for style in [StatusIndicatorStyle::Dots, StatusIndicatorStyle::Symbols] {
        // tab_1 working, tab_2 finished (watched working first), tab_3 idle,
        // all with background subagents; tab_4 blocked, tab_5 suspended and
        // tab_6 unknown, also with subagents; tab_7 idle without.
        let statuses = [
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Idle,
            AgentStatus::Blocked,
            AgentStatus::Suspended,
            AgentStatus::Unknown,
            AgentStatus::Idle,
        ];
        let build = |first_pass: bool| {
            let mut snapshot = two_space_snapshot();
            snapshot.tabs.clear();
            snapshot.agents.clear();
            for (index, status) in statuses.iter().enumerate() {
                let tab_id = format!("tab_{}", index + 1);
                let pane_id = format!("pane_{}", index + 1);
                snapshot.tabs.push(tab(
                    &tab_id,
                    "ws_1",
                    index + 1,
                    &format!("t{index}"),
                    index == 0,
                    *status,
                ));
                let status = if first_pass && *status == AgentStatus::Done {
                    AgentStatus::Working
                } else {
                    *status
                };
                let mut agent = agent(&pane_id, &tab_id, status);
                agent.state_change_seq = if first_pass { 1 } else { 2 };
                agent.subagents = if index == 6 { 0 } else { 2 };
                snapshot.agents.push(agent);
            }
            snapshot
        };
        let mut config = tabs_config();
        config.ui.status_indicators = style;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(build(true)));
        state.set_snapshot(Box::new(build(false)));
        state.set_pane_surface(surface());
        let palette = state.config.palette.clone();
        let frame = state.compose(106, 24).expect("composed frame");
        let cell = |row: usize| {
            let (rect, _) = state.hits.sidebar_tabs[row];
            let cell = &frame.cells[(rect.y * frame.width + rect.x + STATUS_ICON_X) as usize];
            (cell.symbol.clone(), cell.fg)
        };
        let ring = |color| (icon.to_string(), color_to_u32(color));
        assert_eq!(cell(0), ring(palette.yellow), "{style:?}: working");
        assert_eq!(cell(1), ring(palette.teal), "{style:?}: finished");
        assert_eq!(cell(2), ring(palette.green), "{style:?}: idle");
        for (row, what) in [
            (3, "blocked"),
            (4, "suspended"),
            (5, "unknown"),
            (6, "none"),
        ] {
            assert_ne!(cell(row).0, icon, "{style:?}: {what} keeps its icon");
        }
        if style == StatusIndicatorStyle::Symbols {
            assert_eq!(cell(3).0, "\u{00D7}", "blocked keeps ×");
        }
    }
}

#[test]
fn a_multi_pane_tab_sums_its_agents_subagents() {
    let mut snapshot = two_space_snapshot();
    let mut idle = agent("pane_1", "tab_1", AgentStatus::Idle);
    idle.subagents = 2;
    snapshot.agents = vec![idle, agent("pane_1b", "tab_1", AgentStatus::Idle)];
    snapshot.tabs[0].agent_status = AgentStatus::Idle;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[0];
    assert_eq!(
        frame.cells[(rect.y * frame.width + rect.x + STATUS_ICON_X) as usize].symbol,
        super::super::tab_sidebar::TAB_SUBAGENTS_ICON,
        "one agent's background subagents mark the whole tab"
    );
    // A blocked agent in the tab outranks them.
    snapshot.agents[1].agent_status = AgentStatus::Blocked;
    snapshot.tabs[0].agent_status = AgentStatus::Blocked;
    state.set_snapshot(Box::new(snapshot));
    let frame = state.compose(106, 20).expect("composed frame");
    assert_ne!(
        frame.cells[(rect.y * frame.width + rect.x + STATUS_ICON_X) as usize].symbol,
        super::super::tab_sidebar::TAB_SUBAGENTS_ICON
    );
}

fn status_segment(text: &str) -> crate::protocol::ClientShellTabStatusSegment {
    crate::protocol::ClientShellTabStatusSegment {
        text: text.into(),
        accent: false,
    }
}

/// A two-space snapshot whose tab bar status reads `host · 12:00`.
fn status_snapshot() -> ClientShellSnapshot {
    let mut snapshot = two_space_snapshot();
    snapshot.tab_bar_right = vec![status_segment("host"), status_segment("12:00")];
    snapshot.tab_bar_right_separator = " · ".into();
    snapshot
}

fn composed(config: &Config, snapshot: ClientShellSnapshot) -> (ClientShellState, FrameData) {
    composed_at(config, snapshot, 20)
}

/// From this many rows (`tab_sidebar::SPACIOUS_HEIGHT`) the footer draws
/// one row per status line; below it the lines share one row.
const TALL_ROWS: u16 = 40;

fn composed_at(
    config: &Config,
    snapshot: ClientShellSnapshot,
    rows: u16,
) -> (ClientShellState, FrameData) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(wide_surface());
    let frame = state.compose(106, rows).expect("composed frame");
    (state, frame)
}

/// The menu row: the sidebar's last row.
fn menu_y(state: &ClientShellState) -> u16 {
    state.hits.sidebar_divider.bottom() - 1
}

/// The rows the list and the detail strip share between the toolbar and
/// the pinned rows / status footer (the strip's lower rule shows only above
/// a footer).
fn list_and_detail(state: &ClientShellState) -> u16 {
    state.hits.agent_body.height + state.hits.sidebar_detail.height
}

/// The status footer's last row, right above the menu row.
fn status_row(state: &ClientShellState, frame: &FrameData) -> String {
    status_rows(state, frame, 1).remove(0)
}

#[test]
fn tabs_sidebar_footer_shows_the_tab_bar_status_with_its_separator() {
    let (plain, _) = composed(&tabs_config(), two_space_snapshot());
    let (state, frame) = composed(&tabs_config(), status_snapshot());
    let footer = status_row(&state, &frame);
    assert_eq!(footer.trim_end(), " host · 12:00", "{footer:?}");
    assert_eq!(
        list_and_detail(&state) + 1,
        list_and_detail(&plain),
        "the footer takes one row off the list"
    );
    let cell = frame.cells
        [((menu_y(&state) - 1) * frame.width + state.hits.agent_body.x + 1) as usize]
        .clone();
    assert_eq!(
        cell.fg,
        crate::protocol::color_to_u32(state.config.palette.overlay1),
        "dim secondary text"
    );
    assert_eq!(
        frame_rows(&frame)
            .iter()
            .filter(|row| row.contains("host"))
            .count(),
        1,
        "only the footer shows the status in the tabs layout"
    );

    // A same-width status update repaints the footer.
    let mut state = state;
    let mut replacement = status_snapshot();
    replacement.revision = 2;
    replacement.tab_bar_right[1].text = "12:01".into();
    let mut surface = wide_surface();
    surface.projection_revision = 2;
    state.set_snapshot(Box::new(replacement));
    state.set_pane_surface(surface);
    let frame = state.compose(106, 20).expect("updated frame");
    assert_eq!(status_row(&state, &frame).trim_end(), " host · 12:01");
}

#[test]
fn tabs_sidebar_without_status_segments_keeps_the_full_list_height() {
    let (state, frame) = composed(&tabs_config(), two_space_snapshot());
    // 20 rows: one toolbar row and one menu row around the list and the
    // detail strip.
    assert_eq!(list_and_detail(&state), 18);
    assert_eq!(menu_y(&state), 19);
    let body = state.hits.agent_body;
    let menu_row = row_text(
        &frame,
        ratatui::layout::Rect::new(body.x, 19, body.width, 1),
    );
    assert!(
        menu_row.trim_start().starts_with("men"),
        "the row under the strip is the menu row: {menu_row:?}"
    );

    // Segments without text do not open the footer either.
    let mut empty = two_space_snapshot();
    empty.tab_bar_right = vec![status_segment("")];
    empty.tab_bar_right_separator = " · ".into();
    let (state, _) = composed(&tabs_config(), empty);
    assert_eq!(list_and_detail(&state), 18);
}

#[test]
fn tabs_sidebar_footer_truncates_long_status_with_an_ellipsis() {
    let mut snapshot = status_snapshot();
    snapshot.tab_bar_right[1] = status_segment(&"x".repeat(200));
    let (state, frame) = composed(&tabs_config(), snapshot);
    let body = state.hits.agent_body;
    let footer = status_row(&state, &frame);
    assert!(footer.starts_with(" host · xxx"), "{footer:?}");
    assert_eq!(
        footer.chars().nth(usize::from(body.width) - 2),
        Some('…'),
        "{footer:?}"
    );
    assert_eq!(
        footer.chars().last(),
        Some(' '),
        "one-cell right margin: {footer:?}"
    );
}

#[test]
fn tabs_sidebar_footer_never_hides_the_last_tab() {
    let mut snapshot = status_snapshot();
    snapshot.tabs = (1..=40)
        .map(|number| {
            tab(
                &format!("tab_{number}"),
                "ws_1",
                number,
                &format!("t{number}"),
                number == 1,
                AgentStatus::Idle,
            )
        })
        .collect();
    // No group header after the bucket: tab_40 is the list's last row.
    snapshot.workspaces.truncate(1);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(wide_surface());
    state.compose(106, 20).expect("first frame");
    let body = state.hits.agent_body;
    assert_eq!(list_and_detail(&state), 17, "toolbar, footer and menu rows");
    assert!(state.hits.agent_max_scroll > 0);

    // Scroll to the end: the last tab is the list's last visible row.
    state.agent_scroll = usize::MAX;
    let frame = state.compose(106, 20).expect("scrolled frame");
    let (last_rect, last_id) = state
        .hits
        .sidebar_tabs
        .last()
        .expect("visible rows")
        .clone();
    assert_eq!(last_id, "tab_40");
    assert_eq!(last_rect.y, body.bottom() - 1);
    assert!(row_text(&frame, last_rect).contains("t40"));
    assert!(state
        .hits
        .sidebar_tabs
        .iter()
        .all(|(rect, _)| rect.bottom() <= body.bottom()));
    assert_eq!(state.hits.sidebar_tabs.len(), usize::from(body.height));
    assert_eq!(status_row(&state, &frame).trim_end(), " host · 12:00");
}

#[test]
fn spaces_layout_keeps_the_status_in_the_tab_bar() {
    let (state, frame) = composed(&Config::default(), status_snapshot());
    let rows = frame_rows(&frame);
    assert!(rows[0].contains("host · 12:00"), "{:?}", rows[0]);
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("host · 12:00"))
            .count(),
        1,
        "only the tab bar shows it"
    );
    assert!(state.hits.sidebar_tabs.is_empty());
}

/// A status whose command entry keeps three lines with SGR colors, after a
/// plain `host` segment: `host · <truecolor>ok` / `<256>warn` / `<basic>err`.
fn multi_line_status_snapshot() -> ClientShellSnapshot {
    let mut snapshot = two_space_snapshot();
    snapshot.tab_bar_right = vec![
        status_segment("host"),
        status_segment(
            "\x1b[1;38;2;10;20;30mok\x1b[0m\n\x1b[38;5;208mwarn\x1b[39m plain\n\x1b[2;31merr\x1b[22m norm",
        ),
    ];
    snapshot.tab_bar_right_separator = " · ".into();
    snapshot
}

/// The last `count` footer rows, ending right above the menu row.
fn status_rows(state: &ClientShellState, frame: &FrameData, count: u16) -> Vec<String> {
    footer_rows(state, count)
        .rows()
        .map(|rect| row_text(frame, rect))
        .collect()
}

/// The rect of the last `count` footer rows, right above the menu row.
fn footer_rows(state: &ClientShellState, count: u16) -> ratatui::layout::Rect {
    let body = state.hits.agent_body;
    ratatui::layout::Rect::new(body.x, menu_y(state) - count, body.width, count)
}

fn cell_at(
    frame: &FrameData,
    area: ratatui::layout::Rect,
    needle: &str,
) -> crate::protocol::CellData {
    let (x, y) = cell_symbol_position(frame, area, needle);
    frame.cells[(y * frame.width + x) as usize].clone()
}

#[test]
fn tabs_sidebar_footer_draws_one_row_per_status_line_in_its_sgr_styles() {
    use ratatui::style::{Color, Modifier};
    let (plain, _) = composed_at(&tabs_config(), two_space_snapshot(), TALL_ROWS);
    let (state, frame) = composed_at(&tabs_config(), multi_line_status_snapshot(), TALL_ROWS);
    assert_eq!(list_and_detail(&state) + 3, list_and_detail(&plain));
    let rows = status_rows(&state, &frame, 3);
    assert_eq!(rows[0].trim_end(), " host · ok", "{rows:?}");
    assert_eq!(rows[1].trim_end(), " warn plain", "{rows:?}");
    assert_eq!(rows[2].trim_end(), " err norm", "{rows:?}");
    assert!(
        frame_rows(&frame)
            .iter()
            .all(|row| !row.contains('\x1b') && !row.contains("[3")),
        "no raw escapes reach the frame"
    );

    let footer = footer_rows(&state, 3);
    let dim = crate::protocol::color_to_u32(state.config.palette.overlay1);
    let host = cell_at(&frame, footer, "host");
    assert_eq!(host.fg, dim, "unstyled text keeps the footer's dim style");
    let ok = cell_at(&frame, footer, "ok");
    assert_eq!(ok.fg, crate::protocol::color_to_u32(Color::Rgb(10, 20, 30)));
    assert_eq!(ok.modifier & Modifier::BOLD.bits(), Modifier::BOLD.bits());
    let warn = cell_at(&frame, footer, "warn");
    assert_eq!(warn.fg, crate::protocol::color_to_u32(Color::Indexed(208)));
    assert_eq!(
        warn.modifier & Modifier::BOLD.bits(),
        0,
        "each row starts unstyled"
    );
    assert_eq!(
        cell_at(&frame, footer, "plain").fg,
        dim,
        "39 restores the dim fg"
    );
    let err = cell_at(&frame, footer, "err");
    assert_eq!(err.fg, crate::protocol::color_to_u32(Color::Red));
    assert_eq!(err.modifier & Modifier::DIM.bits(), Modifier::DIM.bits());
    let norm = cell_at(&frame, footer, "norm");
    assert_eq!(norm.fg, crate::protocol::color_to_u32(Color::Red));
    assert_eq!(norm.modifier & (Modifier::DIM | Modifier::BOLD).bits(), 0);
}

#[test]
fn tabs_sidebar_footer_truncates_each_styled_row_with_an_ellipsis() {
    let mut snapshot = two_space_snapshot();
    snapshot.tab_bar_right = vec![status_segment(&format!(
        "short\n\x1b[32m{}\x1b[0m{}",
        "g".repeat(5),
        "x".repeat(200)
    ))];
    let (state, frame) = composed_at(&tabs_config(), snapshot, TALL_ROWS);
    let body = state.hits.agent_body;
    let rows = status_rows(&state, &frame, 2);
    assert_eq!(rows[0].trim_end(), " short");
    assert_eq!(
        rows[1].chars().nth(usize::from(body.width) - 2),
        Some('…'),
        "{:?}",
        rows[1]
    );
    assert_eq!(rows[1].chars().last(), Some(' '), "one-cell right margin");
    assert!(rows[1].starts_with(&format!(" {}x", "g".repeat(5))));
    let footer = footer_rows(&state, 2);
    assert_eq!(
        cell_at(&frame, footer, "ggg").fg,
        crate::protocol::color_to_u32(ratatui::style::Color::Green)
    );
    assert_eq!(
        cell_at(&frame, footer, "…").fg,
        crate::protocol::color_to_u32(state.config.palette.overlay1),
        "the ellipsis takes the style of the text it cuts"
    );
}

#[test]
fn tabs_sidebar_footer_skips_empty_lines_and_caps_at_four_rows() {
    let mut snapshot = two_space_snapshot();
    snapshot.tab_bar_right = vec![status_segment("a\n\n\x1b[31m\x1b[0m\n  \nb")];
    let (plain, _) = composed_at(&tabs_config(), two_space_snapshot(), TALL_ROWS);
    let (state, frame) = composed_at(&tabs_config(), snapshot, TALL_ROWS);
    assert_eq!(list_and_detail(&state) + 2, list_and_detail(&plain));
    let rows = status_rows(&state, &frame, 2);
    assert_eq!(rows[0].trim_end(), " a");
    assert_eq!(rows[1].trim_end(), " b");

    let mut snapshot = two_space_snapshot();
    snapshot.tab_bar_right = vec![status_segment("1\n2\n3\n4"), status_segment("5\n6")];
    let (state, frame) = composed_at(&tabs_config(), snapshot, TALL_ROWS);
    assert_eq!(list_and_detail(&state) + 4, list_and_detail(&plain));
    let rows = status_rows(&state, &frame, 4);
    assert_eq!(
        rows.iter().map(|row| row.trim()).collect::<Vec<_>>(),
        vec!["1", "2", "3", "4 5"]
    );
}

#[test]
fn tabs_sidebar_multi_line_footer_never_hides_the_last_tab() {
    let mut snapshot = multi_line_status_snapshot();
    snapshot.tabs = (1..=40)
        .map(|number| {
            tab(
                &format!("tab_{number}"),
                "ws_1",
                number,
                &format!("t{number}"),
                number == 1,
                AgentStatus::Idle,
            )
        })
        .collect();
    snapshot.workspaces.truncate(1);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(wide_surface());
    state.compose(106, 20).expect("first frame");
    let body = state.hits.agent_body;
    assert_eq!(
        list_and_detail(&state),
        17,
        "toolbar, the joined footer row and the menu row"
    );

    state.agent_scroll = usize::MAX;
    let frame = state.compose(106, 20).expect("scrolled frame");
    let (last_rect, last_id) = state
        .hits
        .sidebar_tabs
        .last()
        .expect("visible rows")
        .clone();
    assert_eq!(last_id, "tab_40");
    assert_eq!(last_rect.y, body.bottom() - 1);
    assert!(row_text(&frame, last_rect).contains("t40"));
    assert_eq!(state.hits.sidebar_tabs.len(), usize::from(body.height));
    assert!(
        status_row(&state, &frame).starts_with(" host · ok  warn"),
        "{:?}",
        status_row(&state, &frame)
    );
}

#[test]
fn tabs_sidebar_footer_joins_its_lines_below_30_rows_in_their_colors() {
    use ratatui::style::{Color, Modifier};
    let (plain, _) = composed(&tabs_config(), two_space_snapshot());
    let (state, frame) = composed(&tabs_config(), multi_line_status_snapshot());
    assert_eq!(
        list_and_detail(&state) + 1,
        list_and_detail(&plain),
        "one footer row"
    );
    let row = status_row(&state, &frame);
    assert!(row.starts_with(" host · ok  warn plain"), "{row:?}");
    let footer = footer_rows(&state, 1);
    let ok = cell_at(&frame, footer, "ok");
    assert_eq!(ok.fg, crate::protocol::color_to_u32(Color::Rgb(10, 20, 30)));
    assert_eq!(ok.modifier & Modifier::BOLD.bits(), Modifier::BOLD.bits());
    let warn = cell_at(&frame, footer, "warn");
    assert_eq!(
        warn.fg,
        crate::protocol::color_to_u32(Color::Indexed(208)),
        "each line keeps its own styles"
    );
    assert_eq!(warn.modifier & Modifier::BOLD.bits(), 0);
}

#[test]
fn spaces_tab_bar_shows_a_multi_line_segment_as_its_last_line_without_escapes() {
    let (_, frame) = composed(&Config::default(), multi_line_status_snapshot());
    let rows = frame_rows(&frame);
    assert!(rows[0].contains("host · err norm"), "{:?}", rows[0]);
    assert!(
        rows.iter()
            .all(|row| !row.contains('\x1b') && !row.contains("warn") && !row.contains("[2")),
        "{rows:?}"
    );
    assert_eq!(
        super::super::tab_sidebar::single_line_status_text("plain"),
        std::borrow::Cow::Borrowed("plain")
    );
    assert_eq!(
        super::super::tab_sidebar::single_line_status_text("\x1b[31mred\x1b[0m"),
        "red"
    );
}

/// One frame cell: symbol, fg, bg and whether it is bold.
fn cell_of(frame: &FrameData, x: u16, y: u16) -> (String, u32, u32, bool) {
    let cell = &frame.cells[(y * frame.width + x) as usize];
    (
        cell.symbol.clone(),
        cell.fg,
        cell.bg,
        cell.modifier & ratatui::style::Modifier::BOLD.bits() != 0,
    )
}

fn rgb(color: ratatui::style::Color) -> u32 {
    crate::protocol::color_to_u32(color)
}

#[test]
fn headers_have_no_band_and_bold_names() {
    let mut snapshot = three_space_snapshot();
    // `api` is the focused space; `infra` is a plain open group.
    snapshot.workspaces[1].focused = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 24).expect("composed frame");
    let palette = state.config.palette.clone();
    let (api, _) = state.hits.sidebar_groups[0];
    let (infra, _) = state.hits.sidebar_groups[1];
    for header in [api, infra] {
        for x in header.x..header.right() {
            assert_eq!(
                cell_of(&frame, x, header.y).2,
                rgb(palette.sidebar_bg),
                "no band at x={x}"
            );
        }
        // Marker at 1, name at 5 (bold), count ending at width-4, the
        // rolled-up status at width-2.
        assert_eq!(cell_of(&frame, header.x + 1, header.y).0, "▾");
        assert_eq!(
            cell_of(&frame, header.x + 1, header.y).1,
            rgb(palette.subtext0)
        );
        assert!(cell_of(&frame, header.x + 5, header.y).3, "bold name");
        assert_eq!(cell_of(&frame, header.right() - 4, header.y).0, "1");
        assert_ne!(cell_of(&frame, header.right() - 2, header.y).0, " ");
    }
    assert_eq!(
        cell_of(&frame, api.x + 5, api.y).1,
        rgb(palette.text),
        "the focused group's name is bright"
    );
    assert_eq!(
        cell_of(&frame, infra.x + 5, infra.y).1,
        rgb(palette.subtext0)
    );

    // Hovered: the bar and a bright name, still no band.
    state.sidebar_hover = Some(super::super::sidebar_model::SidebarHover::Group(
        "ws_3".into(),
    ));
    let frame = state.compose(106, 24).expect("composed frame");
    let (infra, _) = state.hits.sidebar_groups[1];
    assert_eq!(
        cell_of(&frame, infra.x, infra.y).0,
        super::super::tab_sidebar::HOVER_BAR
    );
    assert_eq!(cell_of(&frame, infra.x, infra.y).1, rgb(palette.accent));
    assert_eq!(cell_of(&frame, infra.x + 5, infra.y).1, rgb(palette.text));
    assert_eq!(
        cell_of(&frame, infra.x + 5, infra.y).2,
        rgb(palette.sidebar_bg)
    );

    // Folded: `▸` and a dim name.
    state.sidebar_hover = None;
    state.collapsed_groups.insert(group_key("ws_3"));
    state.sidebar_model.mark_dirty();
    let frame = state.compose(106, 24).expect("composed frame");
    let (infra, _) = state.hits.sidebar_groups[1];
    assert_eq!(cell_of(&frame, infra.x + 1, infra.y).0, "▸");
    assert_eq!(
        cell_of(&frame, infra.x + 1, infra.y).1,
        rgb(palette.overlay1)
    );
    assert_eq!(
        cell_of(&frame, infra.x + 5, infra.y).1,
        rgb(palette.overlay1)
    );
    assert!(cell_of(&frame, infra.x + 5, infra.y).3);
}

/// `two_space_snapshot` with `tab_1` (focused, claude) important, on a
/// 30-minute reminder and marked by the browser.
fn marked_snapshot() -> ClientShellSnapshot {
    let mut snapshot = two_space_snapshot();
    snapshot.agents = vec![agent("pane_1", "tab_1", AgentStatus::Idle)];
    snapshot.tabs[0].agent_status = AgentStatus::Idle;
    snapshot.tabs[0].important = true;
    snapshot.tabs[0].remind_every = Some(crate::api::schema::TabRemindInterval::M30);
    snapshot
}

fn marked_state(config: &Config) -> ClientShellState {
    use crate::api::schema::browser::{BrowserGetInfo, BrowserPaneCursor, BrowserProfileInfo};
    let snapshot = marked_snapshot();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.browser.info = Some(BrowserGetInfo {
        seq: 1,
        enabled: true,
        profiles: vec![BrowserProfileInfo {
            name: "main".into(),
            state: "stopped".into(),
            ..Default::default()
        }],
        recent_panes: vec![BrowserPaneCursor {
            pane_id: "pane_1".into(),
            tab_id: Some("tab_1".into()),
            current: "main:t1".into(),
            last_at: crate::app::news::unix_now(),
        }],
        ..Default::default()
    });
    state
}

#[test]
fn marks_pack_flush_right_in_order() {
    let mut state = marked_state(&tabs_config());
    let frame = state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[0];
    let row = row_text(&frame, rect);
    // ◎ ★ ◷ then the focused row's harness glyph, the last at width-2
    // with a one-cell margin.
    assert!(row.ends_with("◎ ★ ◷ \u{29C6} "), "{row:?}");
    assert!(row.starts_with("     ○ reviewer "), "{row:?}");
    let palette = state.config.palette.clone();
    let fg = |offset: u16| cell_of(&frame, rect.right() - offset, rect.y).1;
    assert_eq!(fg(8), rgb(palette.accent), "browser mark");
    assert_eq!(fg(6), rgb(palette.overlay0), "★ unlit");
    assert_eq!(fg(4), rgb(palette.overlay0), "◷ unlit");

    // Hovering another row keeps the glyph here.
    state.sidebar_hover = Some(super::super::sidebar_model::SidebarHover::Tab(
        "tab_3".into(),
    ));
    let frame = state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[0];
    assert!(row_text(&frame, rect).ends_with("◎ ★ ◷ \u{29C6} "));

    // Not focused: the reminder marks move right into the glyph's place.
    let mut snapshot = marked_snapshot();
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_3";
    }
    state.set_snapshot(Box::new(snapshot));
    let frame = state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[0];
    assert!(row_text(&frame, rect).ends_with("◎ ★ ◷ "));

    // Cramped (the narrowest sidebar): no gaps between the marks, so the
    // label keeps more room.
    let mut config = tabs_config();
    config.ui.sidebar_width = 18;
    let mut state = marked_state(&config);
    let frame = state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[0];
    let row = row_text(&frame, rect);
    assert!(row.ends_with("◎★◷\u{29C6} "), "{row:?}");
    assert!(row.contains('…'), "the label truncates: {row:?}");
}

#[test]
fn pinned_rows_align_with_the_headers_and_take_the_hover_band() {
    let mut state = marked_state(&tabs_config());
    let frame = state.compose(106, 20).expect("composed frame");
    let palette = state.config.palette.clone();
    let row = state.hits.browser_row;
    assert_eq!(row.height, 1, "the Browser row is pinned");
    // The glyph in the headers' icon slot, the label at x=5.
    assert_eq!(cell_of(&frame, row.x + 3, row.y).0, "◌");
    assert!(row_text(&frame, row).starts_with("   ◌ Browser"));
    assert_eq!(cell_of(&frame, row.x, row.y).2, rgb(palette.sidebar_bg));

    state.sidebar_hover = Some(super::super::sidebar_model::SidebarHover::Pinned(
        super::super::sidebar_model::PinnedKind::Browser,
    ));
    let frame = state.compose(106, 20).expect("composed frame");
    let row = state.hits.browser_row;
    let (bar, fg, bg, _) = cell_of(&frame, row.x, row.y);
    assert_eq!(bar, super::super::tab_sidebar::HOVER_BAR);
    assert_eq!(fg, rgb(palette.accent));
    assert_eq!(bg, rgb(palette.sidebar_hover_bg()));
    assert_eq!(
        cell_of(&frame, row.right() - 1, row.y).2,
        rgb(palette.sidebar_hover_bg()),
        "the band spans the row"
    );
    // A hovered pinned row leaves the glyph on the focused tab.
    let (rect, _) = state.hits.sidebar_tabs[0];
    assert!(row_text(&frame, rect).ends_with("◎ ★ ◷ \u{29C6} "));
}

#[test]
fn label_emphasis_follows_status() {
    let statuses = [
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Idle,
        AgentStatus::Unknown,
        AgentStatus::Suspended,
    ];
    let mut snapshot = two_space_snapshot();
    snapshot.workspaces.truncate(1);
    snapshot.tabs = std::iter::once(tab("tab_0", "ws_1", 1, "focused", true, AgentStatus::Idle))
        .chain(statuses.iter().enumerate().map(|(index, status)| {
            tab(
                &format!("tab_{}", index + 1),
                "ws_1",
                index + 2,
                &format!("label{}", index + 1),
                false,
                *status,
            )
        }))
        .collect();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    let palette = state.config.palette.clone();
    let label = |row: usize| {
        let (rect, _) = state.hits.sidebar_tabs[row];
        cell_of(&frame, rect.x + 7, rect.y)
    };
    let (_, fg, _, bold) = label(0);
    assert_eq!((fg, bold), (rgb(palette.text), true), "focused: bold text");
    for (row, expected) in [
        (1, palette.text),
        (2, palette.text),
        (3, palette.subtext0),
        (4, palette.subtext0),
        (5, palette.overlay0),
    ] {
        let (_, fg, _, bold) = label(row);
        assert_eq!(fg, rgb(expected), "{:?}", statuses[row - 1]);
        assert!(!bold);
    }
}

/// `groups` groups of `per_group` tabs after a one-tab bucket; `tab_0` is
/// focused.
fn many_groups_snapshot(groups: usize, per_group: usize) -> ClientShellSnapshot {
    let mut snapshot = two_space_snapshot();
    let template = snapshot.workspaces[1].clone();
    snapshot.workspaces.truncate(1);
    snapshot.tabs = vec![tab("tab_0", "ws_1", 1, "home", true, AgentStatus::Idle)];
    for group in 0..groups {
        let mut workspace = template.clone();
        workspace.workspace_id = format!("ws_g{group}");
        workspace.number = group + 2;
        workspace.label = format!("group {group}");
        snapshot.workspaces.push(workspace);
        for index in 0..per_group {
            snapshot.tabs.push(tab(
                &format!("tab_{group}_{index}"),
                &format!("ws_g{group}"),
                index + 1,
                &format!("g{group} t{index}"),
                false,
                AgentStatus::Idle,
            ));
        }
    }
    snapshot
}

#[test]
fn spacer_rows_between_groups_from_30_rows() {
    // Below 30 rows: headers follow the row above directly.
    let state = grouped_state_at(24);
    let (planner, _) = state.hits.sidebar_tabs[1];
    let (api, _) = state.hits.sidebar_groups[0];
    assert_eq!(api.y, planner.y + 1);

    // From 30 rows: one blank row before every header with a row above it.
    let state = grouped_state_at(TALL_ROWS);
    let (planner, _) = state.hits.sidebar_tabs[1];
    let (notes, _) = state.hits.sidebar_tabs[2];
    let (api, _) = state.hits.sidebar_groups[0];
    let (infra, _) = state.hits.sidebar_groups[1];
    assert_eq!(api.y, planner.y + 2, "a spacer above api");
    assert_eq!(notes.y, api.y + 1, "no spacer under a header");
    assert_eq!(infra.y, notes.y + 2, "a spacer above infra");

    // The scroll metrics count the spacers: scrolled to the bottom, the last
    // tab is the list's last visible row.
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(many_groups_snapshot(8, 4)));
    state.set_pane_surface(surface());
    state.compose(106, TALL_ROWS).expect("composed frame");
    let body = state.hits.agent_body;
    assert!(state.hits.agent_max_scroll > 0);
    state.agent_scroll = usize::MAX;
    let frame = state.compose(106, TALL_ROWS).expect("composed frame");
    let (last, last_id) = state.hits.sidebar_tabs.last().cloned().expect("rows");
    assert_eq!(last_id, "tab_7_3");
    assert_eq!(last.y, body.bottom() - 1);
    assert!(row_text(&frame, last).contains("g7 t3"));
    // A header at the top of the view gets no leading blank.
    let top_rows = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| rect.y)
        .chain(state.hits.sidebar_groups.iter().map(|(rect, _)| rect.y));
    assert_eq!(top_rows.min(), Some(body.y));
}

#[test]
fn chrome_rows_use_sidebar_chrome_and_list_keeps_sidebar_bg() {
    let mut snapshot = status_snapshot();
    snapshot.tabs[0].focused = false;
    let check = |state: &ClientShellState, frame: &FrameData, chrome: u32| {
        let palette = &state.config.palette;
        let body = state.hits.agent_body;
        let menu = menu_y(state);
        let content_right = state.hits.sidebar_divider.x;
        for x in body.x..content_right {
            assert_eq!(cell_of(frame, x, 0).2, chrome, "toolbar x={x}");
            assert_eq!(cell_of(frame, x, menu).2, chrome, "menu x={x}");
            assert_eq!(cell_of(frame, x, menu - 1).2, chrome, "footer x={x}");
        }
        let detail = state.hits.sidebar_detail;
        assert!(detail.height > 0, "the detail strip shows at 40 rows");
        for y in detail.y..detail.bottom() {
            assert_eq!(cell_of(frame, body.x, y).2, chrome, "detail y={y}");
        }
        for y in body.y..body.bottom() {
            assert_eq!(
                cell_of(frame, body.x, y).2,
                rgb(palette.sidebar_bg),
                "list y={y}"
            );
        }
    };
    let (state, frame) = composed_at(&tabs_config(), snapshot.clone(), TALL_ROWS);
    let palette = state.config.palette.clone();
    assert_eq!(palette.sidebar_chrome(), palette.surface_dim);
    check(&state, &frame, rgb(palette.surface_dim));

    // `[theme.custom] sidebar_chrome_bg` overrides it.
    let mut config = tabs_config();
    config.theme.custom = Some(crate::config::CustomThemeColors {
        sidebar_chrome_bg: Some("#123456".into()),
        ..Default::default()
    });
    let (state, frame) = composed_at(&config, snapshot.clone(), TALL_ROWS);
    check(
        &state,
        &frame,
        rgb(ratatui::style::Color::Rgb(0x12, 0x34, 0x56)),
    );

    // And per appearance with `[theme.custom.light]` / `.dark`.
    let mut config = tabs_config();
    config.theme.auto_switch = true;
    config.theme.custom = Some(crate::config::CustomThemeColors {
        light: Some(crate::config::ModeThemeColors {
            sidebar_chrome_bg: Some("#e0e0e0".into()),
            ..Default::default()
        }),
        dark: Some(crate::config::ModeThemeColors {
            sidebar_chrome_bg: Some("#101010".into()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let runtime = crate::app::client_theme_runtime_from_config(&config);
    for (appearance, expected) in [
        (
            crate::terminal_theme::HostAppearance::Light,
            ratatui::style::Color::Rgb(0xe0, 0xe0, 0xe0),
        ),
        (
            crate::terminal_theme::HostAppearance::Dark,
            ratatui::style::Color::Rgb(0x10, 0x10, 0x10),
        ),
    ] {
        let palette = crate::app::client_palette_for_appearance(&runtime, appearance);
        assert_eq!(palette.sidebar_chrome(), expected, "{appearance:?}");
        let mut shell_config = ClientShellConfig::from_config(&config);
        shell_config.palette = palette;
        let mut state = ClientShellState::new(shell_config);
        state.set_snapshot(Box::new(snapshot.clone()));
        state.set_pane_surface(wide_surface());
        let frame = state.compose(106, TALL_ROWS).expect("composed frame");
        check(&state, &frame, rgb(expected));
    }
}

#[test]
fn divider_uses_surface1() {
    let (state, frame) = composed_at(&tabs_config(), status_snapshot(), TALL_ROWS);
    let palette = &state.config.palette;
    let divider = state.hits.sidebar_divider;
    assert_eq!(divider.height, TALL_ROWS);
    for y in divider.y..divider.bottom() {
        let (symbol, fg, bg, _) = cell_of(&frame, divider.x, y);
        assert_eq!(symbol, "│", "y={y}");
        assert_eq!(fg, rgb(palette.surface1), "y={y}");
        assert_eq!(bg, rgb(palette.sidebar_bg), "never the chrome: y={y}");
    }
}

mod plan_layout {
    use super::super::super::tab_sidebar::{plan_layout, LayoutWants, SidebarPlan};
    use ratatui::layout::Rect;

    /// An Active agents block of `cap` lines plus its header, `+N more` and
    /// rule.
    fn active(cap: u16) -> u16 {
        cap + 3
    }

    fn plan(height: u16, wants: LayoutWants) -> SidebarPlan {
        let plan = plan_layout(Rect::new(0, 0, 30, height), wants, active);
        // The rows stack top to bottom and fill the content.
        let stack = [
            plan.toolbar,
            plan.active,
            plan.list,
            plan.pinned,
            plan.detail,
            plan.footer,
            plan.menu,
        ];
        for pair in stack.windows(2) {
            assert_eq!(pair[0].bottom(), pair[1].y, "{plan:?}");
        }
        assert_eq!(plan.menu.bottom(), height, "{plan:?}");
        plan
    }

    fn wants(detail_lines: u16, active_cap: u16) -> LayoutWants {
        LayoutWants {
            pinned: 1,
            status_lines: 3,
            active_cap,
            detail_lines,
        }
    }

    #[test]
    fn everything_fits_when_tall() {
        let plan = plan(40, wants(3, 8));
        assert_eq!(plan.active.height, 11);
        assert_eq!(plan.pinned.height, 1);
        assert_eq!(plan.detail.height, 5, "two rules around three lines");
        assert_eq!(plan.footer.height, 3, "one row per line");
        assert_eq!(plan.list.height, 18);
    }

    #[test]
    fn the_footer_joins_below_30_rows_and_the_strip_drops_its_lower_rule_without_it() {
        let plan = plan(20, wants(1, 4));
        assert_eq!(plan.footer.height, 1);
        assert_eq!(plan.detail.height, 3);
        let no_footer = plan_layout(
            Rect::new(0, 0, 30, 20),
            LayoutWants {
                status_lines: 0,
                ..wants(1, 4)
            },
            active,
        );
        assert_eq!(no_footer.footer.height, 0);
        assert_eq!(no_footer.detail.height, 2, "the upper rule and the line");
    }

    #[test]
    fn the_optional_rows_give_way_in_order_for_three_list_rows() {
        // The detail strip goes first.
        let plan = plan(14, wants(1, 4));
        assert_eq!(plan.detail.height, 0);
        assert_eq!(plan.active.height, 7);
        assert_eq!(plan.list.height, 3);
        // Then the Active block's lines step down.
        let plan = plan_layout(Rect::new(0, 0, 30, 12), wants(1, 4), active);
        assert_eq!((plan.detail.height, plan.active.height), (0, 5));
        assert_eq!(plan.list.height, 3);
        // Then the whole block.
        let plan = plan_layout(Rect::new(0, 0, 30, 10), wants(1, 4), active);
        assert_eq!(plan.active.height, 0);
        assert_eq!(plan.footer.height, 1);
        assert_eq!(plan.list.height, 6);
        // Then the footer; the pinned row keeps the list one row.
        let plan = plan_layout(Rect::new(0, 0, 30, 5), wants(1, 4), active);
        assert_eq!(plan.footer.height, 0);
        assert_eq!(plan.pinned.height, 1);
        assert_eq!(plan.list.height, 2);
        let plan = plan_layout(Rect::new(0, 0, 30, 4), wants(1, 4), active);
        assert_eq!((plan.pinned.height, plan.list.height), (1, 1));
    }

    #[test]
    fn a_tall_footer_steps_down_to_one_row_before_none() {
        // 30 rows (one footer row per line), many pinned rows, no block.
        let tight = |pinned| LayoutWants {
            pinned,
            status_lines: 4,
            active_cap: 8,
            detail_lines: 0,
        };
        let plan = plan_layout(Rect::new(0, 0, 30, 30), tight(20), |_| 0);
        assert_eq!((plan.footer.height, plan.list.height), (4, 4));
        let plan = plan_layout(Rect::new(0, 0, 30, 30), tight(24), |_| 0);
        assert_eq!(plan.footer.height, 1, "four rows would leave none");
        assert_eq!(plan.list.height, 3);
        let plan = plan_layout(Rect::new(0, 0, 30, 30), tight(26), |_| 0);
        assert_eq!((plan.footer.height, plan.list.height), (0, 2));
        // The Active block goes before the footer shrinks.
        let plan = plan_layout(Rect::new(0, 0, 30, 30), tight(20), |cap| cap + 3);
        assert_eq!(plan.active.height, 0);
        assert_eq!(plan.footer.height, 4);
    }
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
        let green = crate::protocol::color_to_u32(
            super::super::super::tab_color::tab_color_fg(
                crate::api::schema::TabColor::Green,
                &state.config.palette,
            )
            .unwrap(),
        );
        let fgs = text_fgs(&frame, row, "notes");
        assert!(fgs.iter().all(|fg| *fg == green), "{fgs:?}");
    }

    /// The tab bar's status segments reach the tabs-layout footer. An
    /// upstream change to how the snapshot carries `ui.tab_bar_right` would
    /// silently empty the footer without any merge conflict.
    #[test]
    fn tab_bar_status_reaches_the_tabs_sidebar_footer() {
        let (state, frame) = composed(&tabs_config(), status_snapshot());
        assert_eq!(status_row(&state, &frame).trim_end(), " host · 12:00");
    }

    /// A command entry's multi-line SGR output (`lines`, `ansi`) travels as
    /// one `\n`-joined segment string; the footer must split it into rows and
    /// color them. An upstream change to the segment text path (sanitizing,
    /// joining or drawing it) would flatten or drop the color silently.
    #[test]
    fn colored_multi_line_status_reaches_the_footer_in_color() {
        let (state, frame) = composed_at(&tabs_config(), multi_line_status_snapshot(), TALL_ROWS);
        let footer = footer_rows(&state, 3);
        assert_eq!(
            status_rows(&state, &frame, 3)
                .iter()
                .map(|row| row.trim_end())
                .collect::<Vec<_>>(),
            vec![" host · ok", " warn plain", " err norm"]
        );
        assert_eq!(
            cell_at(&frame, footer, "warn").fg,
            crate::protocol::color_to_u32(ratatui::style::Color::Indexed(208))
        );
    }
}
