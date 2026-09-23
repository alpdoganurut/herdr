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

    assert!(
        state.hits.workspaces.is_empty(),
        "no space rows in tabs layout"
    );
    assert_eq!(state.hits.new_workspace, ratatui::layout::Rect::default());
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
}

#[test]
fn tabs_layout_rows_focus_on_click_and_open_the_tab_menu_on_right_click() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(two_space_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let (rect, _) = state.hits.sidebar_tabs[2];
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
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
    assert!(rows[3].ends_with('\u{25A1}'), "plain shell: {rows:?}");
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
