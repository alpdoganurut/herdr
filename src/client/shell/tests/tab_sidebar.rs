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
            target: ClientContextMenuTarget::Tab { ref tab_id, ref workspace_id },
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
