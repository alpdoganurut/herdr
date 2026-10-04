//! Teams on the client (fork): the `endpoint.teams.v1` state, the group
//! header's mark and purpose, the member rows' dim mark, and the team items
//! of the group and tab menus (gated on `team.get`).

use super::*;
use crate::api::schema::team::{TeamActor, TeamInfo, TeamMemberInfo};
use crate::api::schema::Method;
use crate::config::{Config, SidebarLayoutConfig};
use crate::server::headless::teams::TeamsPayload;
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

pub(super) fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config
}

fn tab(tab_id: &str, workspace_id: &str, label: &str, focused: bool) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: workspace_id.into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused,
        agent_status: AgentStatus::Idle,
        color: None,
        important: false,
        remind_every: None,
    }
}

fn pane(pane_id: &str, workspace_id: &str, tab_id: &str) -> ClientShellPane {
    ClientShellPane {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        label: None,
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: tab_id == "tab_1",
        right_click_passthrough: false,
    }
}

fn agent(pane_id: &str, tab_id: &str, status: AgentStatus) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_2".into(),
        tab_id: tab_id.into(),
        name: None,
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

/// The bucket (`ws_1`, `tab_1`) and one group `search-it` (`ws_2`) with
/// three tabs: two agents (`pane_2`, `pane_3`) and a shell (`pane_4`).
pub(super) fn team_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let mut group = snapshot.workspaces[0].clone();
    group.workspace_id = "ws_2".into();
    group.active_tab_id = "tab_2".into();
    group.number = 2;
    group.label = "search-it".into();
    group.focused = false;
    snapshot.workspaces.push(group);
    snapshot.tabs = vec![
        tab("tab_1", "ws_1", "home", true),
        tab("tab_2", "ws_2", "fixer", false),
        tab("tab_3", "ws_2", "helper", false),
        tab("tab_4", "ws_2", "shell", false),
    ];
    snapshot.panes = vec![
        pane("pane_1", "ws_1", "tab_1"),
        pane("pane_2", "ws_2", "tab_2"),
        pane("pane_3", "ws_2", "tab_3"),
        pane("pane_4", "ws_2", "tab_4"),
    ];
    snapshot.agents = vec![
        agent("pane_2", "tab_2", AgentStatus::Working),
        agent("pane_3", "tab_3", AgentStatus::Idle),
    ];
    snapshot
}

/// The `search-it` team: `fixer` (`pane_2`) and a member without a role
/// (`pane_3`); `w2:p5` was removed.
pub(super) fn team(purpose: Option<&str>) -> TeamInfo {
    TeamInfo {
        workspace_id: "ws_2".into(),
        workspace_label: "search-it".into(),
        purpose: purpose.map(str::to_owned),
        purpose_by: purpose.map(|_| TeamActor::User),
        created_unix: 1,
        revision: 3,
        members: vec![
            TeamMemberInfo {
                pane_id: "pane_2".into(),
                tab_id: Some("tab_2".into()),
                name: Some("fixer".into()),
                agent: Some("claude".into()),
                role: Some("fixer".into()),
                joined_unix: 1,
                ..TeamMemberInfo::default()
            },
            TeamMemberInfo {
                pane_id: "pane_3".into(),
                tab_id: Some("tab_3".into()),
                agent: Some("codex".into()),
                joined_unix: 2,
                ..TeamMemberInfo::default()
            },
        ],
        excluded: vec!["w2:p5".into()],
    }
}

/// A push, built from its JSON shape (`boot_id`, `revision`, `teams`).
pub(super) fn payload(boot_id: &str, revision: u64, teams: Vec<TeamInfo>) -> TeamsPayload {
    serde_json::from_value(serde_json::json!({
        "boot_id": boot_id,
        "revision": revision,
        "teams": teams,
    }))
    .expect("teams payload")
}

pub(super) fn team_state(purpose: Option<&str>) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    assert!(state.receive_teams(
        &ClientEndpointId::Local,
        payload("boot-1", 1, vec![team(purpose)])
    ));
    state.compose(106, 24).expect("composed frame");
    state
}

fn row_text(frame: &FrameData, rect: Rect) -> String {
    (rect.x..rect.right())
        .map(|x| {
            frame.cells[(rect.y * frame.width + x) as usize]
                .symbol
                .as_str()
        })
        .collect()
}

fn header(state: &mut ClientShellState) -> (String, Rect, FrameData) {
    let frame = state.compose(106, 24).expect("composed frame");
    let (rect, _) = state.hits.sidebar_groups[0];
    (row_text(&frame, rect), rect, frame)
}

fn tab_row(state: &ClientShellState, frame: &FrameData, tab_id: &str) -> String {
    let (rect, _) = state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .expect("tab row");
    row_text(frame, *rect)
}

pub(super) fn endpoint_requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

pub(super) fn mouse(kind: MouseEventKind, column: u16, row: u16) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })
}

fn group_menu(state: &mut ClientShellState) -> Vec<(String, ClientContextMenuAction)> {
    state.overlay = None;
    let (_, rect, _) = header(state);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Right),
        rect.x + 3,
        rect.y,
    )]);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .into_iter()
            .map(|item| (item.label.to_owned(), item.action))
            .collect(),
        _ => panic!("group menu"),
    }
}

fn tab_menu(state: &mut ClientShellState, tab_id: &str) -> Vec<(String, ClientContextMenuAction)> {
    state.overlay = None;
    state.compose(106, 24).expect("composed frame");
    let (rect, _) = *state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .expect("tab row");
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Right),
        rect.x + 2,
        rect.y,
    )]);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .into_iter()
            .map(|item| (item.label.to_owned(), item.action))
            .collect(),
        _ => panic!("tab menu"),
    }
}

/// Activate the open menu's item with `action`.
fn pick(state: &mut ClientShellState, action: ClientContextMenuAction) -> ClientShellInput {
    let index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == action)
            .expect("menu item"),
        _ => panic!("menu"),
    };
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    outcome
}

fn team_methods() -> Vec<String> {
    crate::api::schema::team::method::CLIENT_SHELL
        .iter()
        .map(|name| (*name).to_owned())
        .chain(["pane.move".to_owned(), "tab.focus".to_owned()])
        .collect()
}

#[test]
fn the_header_shows_the_mark_and_the_purpose() {
    let mut state = team_state(Some("fix sync"));
    let (text, rect, frame) = header(&mut state);
    assert!(text.contains("▾ ◆ fix sync"), "{text:?}");
    assert!(
        !text.contains("search-it"),
        "the purpose replaces the label"
    );
    assert!(text.contains('3'), "the member count stays: {text:?}");
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let mark_x = rect.x + 3;
    assert_eq!(buffer[(mark_x, rect.y)].symbol(), "◆");
    assert_eq!(buffer[(mark_x, rect.y)].fg, state.config.palette.accent);
    assert_ne!(
        buffer[(mark_x + 2, rect.y)].fg,
        state.config.palette.overlay0
    );
}

#[test]
fn the_header_shows_the_group_label_dim_until_there_is_a_purpose() {
    let mut state = team_state(None);
    let (text, rect, frame) = header(&mut state);
    assert!(text.contains("▾ ◆ search-it"), "{text:?}");
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let palette = &state.config.palette;
    assert_eq!(buffer[(rect.x + 3, rect.y)].fg, palette.overlay0);
    assert_eq!(buffer[(rect.x + 5, rect.y)].fg, palette.overlay0);
}

#[test]
fn a_group_without_a_team_keeps_its_plain_header() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    let (text, _, _) = header(&mut state);
    assert!(
        text.contains("▾   search-it") && !text.contains('◆'),
        "{text:?}"
    );
}

#[test]
fn team_mark_only_on_the_header() {
    // Sidebar v2: the team's `◆` sits on its header only. Agents model v2:
    // every tab is part of herdr+, so there is no `+`.
    let mut state = team_state(Some("ship it"));
    let frame = state.compose(106, 24).expect("composed frame");
    for tab_id in ["tab_1", "tab_2", "tab_3", "tab_4"] {
        let row = tab_row(&state, &frame, tab_id);
        assert!(
            !row.contains('◆') && !row.contains('+'),
            "{tab_id}: {row:?}"
        );
    }
    let (text, _, _) = header(&mut state);
    assert!(text.contains("▾ ◆ ship it"), "{text:?}");
}

#[test]
fn the_maps_are_built_once_per_push_and_answer_in_constant_time() {
    let state = team_state(Some("ship it"));
    let teams = state.active_teams().expect("teams");
    assert_eq!(teams.by_workspace.get("ws_2"), Some(&0));
    assert!(teams.is_member_tab("tab_2") && teams.is_member_tab("tab_3"));
    assert!(!teams.is_member_tab("tab_4"));
    assert!(teams.team("ws_1").is_none());
}

#[test]
fn an_older_push_is_ignored_and_a_new_server_replaces_the_state() {
    let mut state = team_state(Some("first"));
    assert!(!state.receive_teams(
        &ClientEndpointId::Local,
        payload("boot-1", 1, vec![team(Some("stale"))])
    ));
    assert!(!state.receive_teams(&ClientEndpointId::Local, payload("boot-1", 0, Vec::new())));
    assert_eq!(
        state.active_team("ws_2").and_then(|t| t.purpose.as_deref()),
        Some("first")
    );
    assert!(state.receive_teams(&ClientEndpointId::Local, payload("boot-1", 2, Vec::new())));
    assert!(state.active_team("ws_2").is_none());
    let (text, _, _) = header(&mut state);
    assert!(!text.contains('◆'), "disbanded: {text:?}");
}

#[test]
fn teams_of_another_server_are_never_drawn() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    state.receive_teams(
        &ClientEndpointId::Local,
        payload("boot-0", 9, vec![team(Some("old server"))]),
    );
    assert!(state.active_teams().is_none());
    let (text, _, _) = header(&mut state);
    assert!(!text.contains('◆'), "{text:?}");
}

#[test]
fn render_does_no_team_work_without_teams() {
    // Without a push, and with an empty one, the frame is the plain one.
    let mut plain = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    plain.set_snapshot(Box::new(team_snapshot()));
    plain.set_pane_surface(surface());
    let before = plain.compose(106, 24).expect("frame");
    assert!(plain.active_teams().is_none());
    plain.receive_teams(&ClientEndpointId::Local, payload("boot-1", 1, Vec::new()));
    let after = plain.compose(106, 24).expect("frame");
    assert_eq!(frame_rows(&before), frame_rows(&after));
    let teams = plain.active_teams().expect("teams");
    assert!(teams.by_workspace.is_empty() && teams.member_tabs.is_empty());
}

#[test]
fn group_menu_items_are_gated_on_team_get() {
    use ClientContextMenuAction as Action;
    let mut state = team_state(Some("ship it"));
    state.set_endpoint_methods(Some(vec!["pane.move".into(), "tab.focus".into()]));
    let actions: Vec<_> = group_menu(&mut state).into_iter().map(|(_, a)| a).collect();
    assert_eq!(
        actions,
        [Action::Rename, Action::Ungroup, Action::CloseGroup]
    );
    let labels: Vec<_> = group_menu(&mut state).into_iter().map(|(l, _)| l).collect();
    assert_eq!(labels[1], "Ungroup", "no relabel without team.get");

    state.set_endpoint_methods(Some(team_methods()));
    let items = group_menu(&mut state);
    assert_eq!(
        items,
        [
            ("Rename group".to_owned(), Action::Rename),
            ("Ungroup (disbands team)".to_owned(), Action::Ungroup),
            ("Close group".to_owned(), Action::CloseGroup),
            ("Team info".to_owned(), Action::TeamInfo),
            ("Edit purpose…".to_owned(), Action::EditPurpose),
            ("Disband team".to_owned(), Action::DisbandTeam),
        ]
    );

    // Not a team yet: "Make team…".
    state.receive_teams(&ClientEndpointId::Local, payload("boot-1", 5, Vec::new()));
    let actions: Vec<_> = group_menu(&mut state).into_iter().map(|(_, a)| a).collect();
    assert_eq!(
        actions,
        [
            Action::Rename,
            Action::Ungroup,
            Action::CloseGroup,
            Action::MakeTeam
        ]
    );
    // The ungrouped bucket never is a team.
    assert_eq!(state.group_team_menu("ws_1"), None);
}

#[test]
fn make_team_prompts_for_an_optional_purpose_and_sends_team_make() {
    let mut state = team_state(None);
    state.receive_teams(&ClientEndpointId::Local, payload("boot-1", 5, Vec::new()));
    group_menu(&mut state);
    pick(&mut state, ClientContextMenuAction::MakeTeam);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "team purpose (optional)",
            target: ClientRenameTarget::TeamPurpose { ref workspace_id, make: true, reopen: false },
            ..
        })) if workspace_id == "ws_2"
    ));
    let outcome = state.handle_input_bytes(b"fix calendar sync\r");
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamMake(params))] = &requests[..] else {
        panic!("team.make, got {requests:?}");
    };
    assert_eq!(params.workspace_id, "ws_2");
    assert_eq!(params.purpose.as_deref(), Some("fix calendar sync"));
    assert_eq!(params.caller_pane, None, "the TUI is the user");
    assert!(state.overlay.is_none());

    // An empty purpose makes the team without one.
    group_menu(&mut state);
    pick(&mut state, ClientContextMenuAction::MakeTeam);
    let outcome = state.handle_input_bytes(b"\r");
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamMake(params))] = &requests[..] else {
        panic!("team.make");
    };
    assert_eq!(params.purpose, None);
}

#[test]
fn edit_purpose_and_disband_send_their_methods_without_a_confirmation() {
    let mut state = team_state(Some("old"));
    group_menu(&mut state);
    pick(&mut state, ClientContextMenuAction::EditPurpose);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Rename(rename)) => {
            assert_eq!(rename.input.as_str(), "old");
            assert!(matches!(
                rename.target,
                ClientRenameTarget::TeamPurpose { make: false, .. }
            ));
        }
        _ => panic!("purpose prompt"),
    }
    let mut outcome = state.handle_input_bytes(b"\x15");
    outcome
        .actions
        .extend(state.handle_input_bytes(b"new aim\r").actions);
    let requests = endpoint_requests(&outcome);
    assert!(
        requests.iter().any(|(_, method)| matches!(
            method,
            Method::TeamSetPurpose(params)
                if params.workspace_id == "ws_2" && params.purpose.is_some()
        )),
        "{requests:?}"
    );

    group_menu(&mut state);
    let outcome = pick(&mut state, ClientContextMenuAction::DisbandTeam);
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamDisband(params))] = &requests[..] else {
        panic!("team.disband, got {requests:?}");
    };
    assert_eq!(params.workspace_id, "ws_2");
    assert!(state.overlay.is_none(), "no confirmation");
}

#[test]
fn ungroup_on_a_team_group_is_relabelled_and_confirmed() {
    let mut state = team_state(Some("ship it"));
    group_menu(&mut state);
    let outcome = pick(&mut state, ClientContextMenuAction::Ungroup);
    assert!(endpoint_requests(&outcome).is_empty(), "nothing moves yet");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(ClientConfirmCloseOverlay {
            ungroup: true,
            ref workspace_id,
            ref detail,
            ..
        })) if workspace_id == "ws_2" && detail.contains("disbanded")
    ));
    // Esc keeps everything.
    let outcome = state.handle_input_bytes(b"\x1b");
    assert!(endpoint_requests(&outcome).is_empty());

    group_menu(&mut state);
    pick(&mut state, ClientContextMenuAction::Ungroup);
    let outcome = state.handle_input_bytes(b"\r");
    let moves: Vec<_> = endpoint_requests(&outcome)
        .into_iter()
        .filter_map(|(_, method)| match method {
            Method::PaneMove(params) => Some(params.pane_id),
            _ => None,
        })
        .collect();
    assert_eq!(moves, ["pane_2", "pane_3", "pane_4"]);
    assert!(
        !endpoint_requests(&outcome)
            .iter()
            .any(|(_, method)| matches!(method, Method::WorkspaceClose(_))),
        "an ungroup never closes"
    );

    // Like Close group, `confirm_close = false` skips the confirmation.
    let mut state = team_state(Some("ship it"));
    state.config.confirm_close = false;
    group_menu(&mut state);
    let outcome = pick(&mut state, ClientContextMenuAction::Ungroup);
    assert!(state.overlay.is_none(), "no confirmation");
    assert_eq!(
        endpoint_requests(&outcome)
            .into_iter()
            .filter(|(_, method)| matches!(method, Method::PaneMove(_)))
            .count(),
        3
    );
}

#[test]
fn the_tab_menu_offers_role_leave_or_join_in_a_team_group() {
    use ClientContextMenuAction as Action;
    let mut state = team_state(Some("ship it"));
    let actions: Vec<_> = tab_menu(&mut state, "tab_2")
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    let close = actions
        .iter()
        .position(|a| *a == Action::Close)
        .expect("close");
    assert_eq!(
        &actions[close + 1..close + 4],
        [Action::SetRole, Action::LeaveTeam, Action::ToggleInfoPane],
        "right after Close, then the tab's Info pane"
    );
    assert!(!actions.contains(&Action::JoinTeam));
    assert!(
        !actions.contains(&Action::SetTeamRole),
        "one role item: Set role… replaces Set team role…"
    );
    assert_eq!(
        actions.last(),
        Some(&Action::Color),
        "the swatch row stays last"
    );

    // A shell tab has no agent: no role or team items, but the Info pane.
    let actions: Vec<_> = tab_menu(&mut state, "tab_4")
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    assert!(!actions.iter().any(|a| matches!(
        a,
        Action::SetRole | Action::SetTeamRole | Action::LeaveTeam | Action::JoinTeam
    )));
    let close = actions
        .iter()
        .position(|a| *a == Action::Close)
        .expect("close");
    assert_eq!(actions[close + 1], Action::ToggleInfoPane);

    // An agent that is not a member can join, and still has a role.
    let mut removed = team(Some("ship it"));
    removed.members.truncate(1);
    state.receive_teams(
        &ClientEndpointId::Local,
        payload("boot-1", 4, vec![removed]),
    );
    let actions: Vec<_> = tab_menu(&mut state, "tab_3")
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    let close = actions
        .iter()
        .position(|a| *a == Action::Close)
        .expect("close");
    assert_eq!(
        &actions[close + 1..close + 4],
        [Action::SetRole, Action::JoinTeam, Action::ToggleInfoPane]
    );
    let outcome = pick(&mut state, Action::JoinTeam);
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamJoin(params))] = &requests[..] else {
        panic!("team.join only (no focus), got {requests:?}");
    };
    assert_eq!((params.pane_id.as_str(), &params.role), ("pane_3", &None));

    // A server with teams but without `agents.set_meta`: a member keeps
    // "Set team role…", a non-member gets no role item.
    state.set_endpoint_methods(Some(team_methods()));
    let actions: Vec<_> = tab_menu(&mut state, "tab_2")
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    let close = actions
        .iter()
        .position(|a| *a == Action::Close)
        .expect("close");
    assert_eq!(
        &actions[close + 1..close + 4],
        [
            Action::SetTeamRole,
            Action::LeaveTeam,
            Action::ToggleInfoPane
        ]
    );
    let actions: Vec<_> = tab_menu(&mut state, "tab_3")
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    assert!(!actions.contains(&Action::SetRole) && !actions.contains(&Action::SetTeamRole));
    assert!(actions.contains(&Action::JoinTeam));

    // Without team.get there are no team items.
    state.set_endpoint_methods(Some(vec!["tab.focus".into()]));
    let actions: Vec<_> = tab_menu(&mut state, "tab_2")
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    assert!(!actions.contains(&Action::SetTeamRole));
    assert!(!actions.contains(&Action::SetRole));
    assert!(!actions.contains(&Action::LeaveTeam));
}

#[test]
fn set_team_role_and_leave_send_their_methods() {
    // A server without `agents.set_meta` (else "Set role…" is offered).
    let mut state = team_state(Some("ship it"));
    state.set_endpoint_methods(Some(team_methods()));
    tab_menu(&mut state, "tab_2");
    let outcome = pick(&mut state, ClientContextMenuAction::LeaveTeam);
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamLeave(params))] = &requests[..] else {
        panic!("team.leave, got {requests:?}");
    };
    assert_eq!(params.pane_id, "pane_2");

    tab_menu(&mut state, "tab_2");
    pick(&mut state, ClientContextMenuAction::SetTeamRole);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Rename(rename)) => {
            assert_eq!(rename.input.as_str(), "fixer", "the current role");
            assert!(matches!(
                rename.target,
                ClientRenameTarget::TeamRole { ref pane_id, reopen: false, .. } if pane_id == "pane_2"
            ));
        }
        _ => panic!("role prompt"),
    }
    state.handle_input_bytes(b"\x15");
    let outcome = state.handle_input_bytes(b"code reviewer\r");
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamSetRole(params))] = &requests[..] else {
        panic!("team.set_role, got {requests:?}");
    };
    assert_eq!(params.pane_id, "pane_2");
    assert_eq!(params.role.as_deref(), Some("code reviewer"));
    assert!(state.overlay.is_none(), "no Team info to reopen");

    // An empty role clears it.
    tab_menu(&mut state, "tab_2");
    pick(&mut state, ClientContextMenuAction::SetTeamRole);
    state.handle_input_bytes(b"\x15");
    let outcome = state.handle_input_bytes(b"\r");
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamSetRole(params))] = &requests[..] else {
        panic!("team.set_role");
    };
    assert_eq!(params.role, None);
}

#[test]
fn one_line_keeps_the_first_line_trimmed() {
    use super::super::teams::one_line;
    assert_eq!(one_line("  ship it \nsecond"), Some("ship it".into()));
    assert_eq!(one_line("   "), None);
    assert_eq!(one_line(""), None);
}
