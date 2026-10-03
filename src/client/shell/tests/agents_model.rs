//! Agents model v2 on the client (fork): "Set role…" on any agent tab
//! (`agents.set_meta`, gated on the server advertising it, with the
//! `team.set_role` fallback for members), and the Team info rights lines.
//! The tab menu's "Info pane" is covered in `info_dock.rs`, the dropped `+`
//! mark in `teams.rs` and `coordinator.rs`.

use super::teams::{endpoint_requests, payload, tabs_config, team, team_snapshot, team_state};
use super::*;
use crate::api::schema::agents_model::AgentsSetMetaParams;
use crate::api::schema::Method;

/// The team methods plus focus and move: a teams server without
/// `agents.set_meta`.
fn v1_team_methods() -> Vec<String> {
    crate::api::schema::team::method::CLIENT_SHELL
        .iter()
        .map(|name| (*name).to_owned())
        .chain(["pane.move".to_owned(), "tab.focus".to_owned()])
        .collect()
}

/// `team_snapshot` with no team pushed: every agent tab is outside a team.
fn plain_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("composed frame");
    state
}

fn menu_actions(state: &mut ClientShellState, tab_id: &str) -> Vec<ClientContextMenuAction> {
    state.overlay = None;
    state.open_tab_context_menu(tab_id.into(), 10, 5);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            menu.items().into_iter().map(|item| item.action).collect()
        }
        _ => panic!("tab menu"),
    }
}

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

/// Open "Set role…" on `tab_id`, clear the prompt, type `text` and save.
fn save_role(state: &mut ClientShellState, tab_id: &str, text: &str) -> ClientShellInput {
    menu_actions(state, tab_id);
    pick(state, ClientContextMenuAction::SetRole);
    state.handle_input_bytes(b"\x15");
    state.handle_input_bytes(format!("{text}\r").as_bytes())
}

fn set_meta(outcome: &ClientShellInput) -> AgentsSetMetaParams {
    let requests = endpoint_requests(outcome);
    let [(_, Method::AgentsSetMeta(params))] = &requests[..] else {
        panic!("agents.set_meta only, got {requests:?}");
    };
    params.clone()
}

#[test]
fn set_role_is_offered_on_an_agent_tab_outside_a_team() {
    use ClientContextMenuAction as Action;
    let mut state = plain_state();
    let actions = menu_actions(&mut state, "tab_2");
    let close = actions
        .iter()
        .position(|a| *a == Action::Close)
        .expect("close");
    assert_eq!(
        &actions[close + 1..close + 4],
        [Action::SetRole, Action::ToggleInfoPane, Action::Important]
    );
    assert!(!actions.iter().any(|a| matches!(
        a,
        Action::SetTeamRole | Action::LeaveTeam | Action::JoinTeam
    )));

    // A shell tab has no role item.
    assert!(!menu_actions(&mut state, "tab_4").contains(&Action::SetRole));

    // The coordinator tab never offers a role.
    state.coordinator.info = Some(crate::api::schema::coordinator::CoordinatorGetInfo {
        tab_id: Some("tab_2".into()),
        ..Default::default()
    });
    assert!(!menu_actions(&mut state, "tab_2").contains(&Action::SetRole));
    assert!(menu_actions(&mut state, "tab_3").contains(&Action::SetRole));
    state.coordinator.info = None;

    // Without `agents.set_meta` a non-member gets no role item.
    state.set_endpoint_methods(Some(v1_team_methods()));
    assert!(!menu_actions(&mut state, "tab_2").contains(&Action::SetRole));
}

#[test]
fn a_non_members_role_is_a_label_sent_through_set_meta() {
    let mut state = plain_state();
    menu_actions(&mut state, "tab_2");
    let outcome = pick(&mut state, ClientContextMenuAction::SetRole);
    assert!(
        endpoint_requests(&outcome).is_empty(),
        "the prompt opens without focusing the tab"
    );
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Rename(rename)) => {
            assert_eq!(rename.input.as_str(), "", "the client never sees it");
            assert!(!rename.title.contains("renamed"), "{}", rename.title);
            assert!(matches!(
                rename.target,
                ClientRenameTarget::AgentRole {
                    ref pane_id,
                    member: false,
                    known: false,
                } if pane_id == "pane_2"
            ));
        }
        _ => panic!("role prompt"),
    }
    let outcome = state.handle_input_bytes(b"reviewer\r");
    assert_eq!(
        set_meta(&outcome),
        AgentsSetMetaParams {
            caller_pane: None,
            target: "pane_2".into(),
            role: Some("reviewer".into()),
            note: None,
        }
    );
    assert!(state.overlay.is_none());

    // An empty save of an unknown role sends nothing (no blind clear).
    let outcome = save_role(&mut state, "tab_2", "");
    assert!(endpoint_requests(&outcome).is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn a_members_role_prefills_and_an_empty_save_clears_it() {
    let mut state = team_state(Some("ship it"));
    menu_actions(&mut state, "tab_2");
    pick(&mut state, ClientContextMenuAction::SetRole);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Rename(rename)) => {
            assert_eq!(rename.input.as_str(), "fixer", "the current role");
            assert!(rename.title.contains("renamed"), "{}", rename.title);
            assert!(matches!(
                rename.target,
                ClientRenameTarget::AgentRole {
                    member: true,
                    known: true,
                    ..
                }
            ));
        }
        _ => panic!("role prompt"),
    }
    state.handle_input_bytes(b"\x15");
    let outcome = state.handle_input_bytes(b"code reviewer\r");
    let params = set_meta(&outcome);
    assert_eq!(params.target, "pane_2");
    assert_eq!(params.role.as_deref(), Some("code reviewer"));
    assert_eq!(params.caller_pane, None, "the user");

    let outcome = save_role(&mut state, "tab_2", "");
    assert_eq!(set_meta(&outcome).role.as_deref(), Some(""), "clears it");

    // A member without a role: an empty save clears (a no-op) through
    // set_meta too; the role is known (none).
    let outcome = save_role(&mut state, "tab_3", "");
    assert_eq!(set_meta(&outcome).role.as_deref(), Some(""));
}

#[test]
fn without_set_meta_at_save_a_member_falls_back_to_team_set_role() {
    let mut state = team_state(Some("ship it"));
    menu_actions(&mut state, "tab_2");
    pick(&mut state, ClientContextMenuAction::SetRole);
    // The server changed under the open prompt.
    state.set_endpoint_methods(Some(v1_team_methods()));
    state.handle_input_bytes(b"\x15");
    let outcome = state.handle_input_bytes(b"lead\r");
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamSetRole(params))] = &requests[..] else {
        panic!("team.set_role, got {requests:?}");
    };
    assert_eq!(params.pane_id, "pane_2");
    assert_eq!(params.role.as_deref(), Some("lead"));
    assert_eq!(params.caller_pane, None);

    // A non-member's label cannot be set there: nothing is sent.
    let mut removed = team(Some("ship it"));
    removed.members.truncate(1);
    let mut state = team_state(Some("ship it"));
    state.receive_teams(
        &ClientEndpointId::Local,
        payload("boot-1", 4, vec![removed]),
    );
    menu_actions(&mut state, "tab_3");
    pick(&mut state, ClientContextMenuAction::SetRole);
    state.set_endpoint_methods(Some(v1_team_methods()));
    let outcome = state.handle_input_bytes(b"helper\r");
    assert!(endpoint_requests(&outcome).is_empty());
}

fn screen_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 24).expect("composed frame");
    (0..frame.height)
        .map(|y| {
            (0..frame.width)
                .map(|x| frame.cells[(y * frame.width + x) as usize].symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn team_info_names_the_rights_only_on_a_v2_server() {
    use super::super::team_overlay::RIGHTS_FOOTER;
    let mut state = team_state(Some("ship it"));
    state.open_team_overlay("ws_2".into());
    let text = screen_text(&mut state);
    for line in RIGHTS_FOOTER {
        assert!(text.contains(line), "{line:?} in\n{text}");
    }
    let rows: Vec<&str> = text.lines().collect();
    let hint = rows
        .iter()
        .position(|row| row.contains("click a member to focus it"))
        .expect("hint row");
    assert!(
        rows[hint - 1].contains(RIGHTS_FOOTER[1]),
        "right above the hint"
    );
    assert!(rows[hint - 2].contains(RIGHTS_FOOTER[0]));

    let mut state = team_state(Some("ship it"));
    state.set_endpoint_methods(Some(v1_team_methods()));
    state.open_team_overlay("ws_2".into());
    let text = screen_text(&mut state);
    assert!(text.contains("click a member to focus it"), "{text}");
    for line in RIGHTS_FOOTER {
        assert!(!text.contains(line), "{line:?} in\n{text}");
    }
}
