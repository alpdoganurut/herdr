//! The Team info overlay (fork): opened from the group menu, it shows the
//! push's membership, pulls `team.get` (on open, after a push for its group,
//! once a minute) and renders status and time in state from the reply; `✎`
//! opens the Rename modal and comes back, `×` and `+ join` send their
//! methods, a member row focuses its pane.

use super::teams::{endpoint_requests, mouse, payload, team, team_state};
use super::*;
use crate::api::schema::{Method, ResponseResult};
use crossterm::event::{MouseButton, MouseEventKind};

fn open_overlay(state: &mut ClientShellState) {
    state.open_team_overlay("ws_2".into());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::TeamInfo(_))
    ));
}

fn overlay(state: &ClientShellState) -> &super::super::team_overlay::ClientTeamOverlay {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::TeamInfo(overlay)) => overlay,
        _ => panic!("team overlay"),
    }
}

/// Run the tick; returns the `team.get` request ids it sent.
fn tick(state: &mut ClientShellState, now: std::time::Instant) -> Vec<String> {
    let mut outcome = ClientShellInput::default();
    state.tick_teams(now, &mut outcome);
    endpoint_requests(&outcome)
        .into_iter()
        .filter_map(|(id, method)| match method {
            Method::TeamGet(params) => {
                assert_eq!(params.workspace_id.as_deref(), Some("ws_2"));
                assert_eq!(params.caller_pane, None);
                Some(id)
            }
            _ => None,
        })
        .collect()
}

fn detailed_team() -> crate::api::schema::team::TeamInfo {
    let mut detailed = team(Some("fix calendar sync"));
    let now = super::super::teams::unix_now();
    detailed.members[0].status = Some(AgentStatus::Working);
    detailed.members[0].status_since_unix = Some(now.saturating_sub(240));
    detailed.members[1].status = Some(AgentStatus::Idle);
    detailed.members[1].status_since_unix = Some(now.saturating_sub(720));
    detailed
}

fn reply(state: &mut ClientShellState, id: &str, team: Option<crate::api::schema::team::TeamInfo>) {
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        id,
        Ok(ResponseResult::TeamReply {
            team,
            renamed: None,
        }),
    );
    assert!(repaint);
}

fn text(state: &mut ClientShellState) -> String {
    let frame = state.compose(120, 30).expect("frame");
    frame_rows(&frame).join("\n")
}

fn hit(state: &ClientShellState, wanted: &super::super::team_overlay::TeamOverlayHit) -> Rect {
    state
        .hits
        .team_overlay
        .iter()
        .find(|(_, hit)| hit == wanted)
        .map(|(rect, _)| *rect)
        .expect("overlay hit")
}

fn click_on(
    state: &mut ClientShellState,
    wanted: &super::super::team_overlay::TeamOverlayHit,
) -> ClientShellInput {
    let rect = hit(state, wanted);
    click(state, rect)
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        rect.x,
        rect.y,
    )])
}

#[test]
fn it_opens_from_the_group_menu_and_pulls_team_get() {
    let mut state = team_state(Some("fix calendar sync"));
    state.overlay = None;
    let (rect, _) = state.hits.sidebar_groups[0];
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Right),
        rect.x + 3,
        rect.y,
    )]);
    let index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::TeamInfo)
            .expect("Team info"),
        _ => panic!("menu"),
    };
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(endpoint_requests(&outcome).is_empty(), "the tick pulls");
    // Until the reply: the push's membership, no status.
    let screen = text(&mut state);
    assert!(screen.contains("team · search-it"), "{screen}");
    assert!(screen.contains("fix calendar sync"), "{screen}");
    assert!(screen.contains("◆ fixer"), "{screen}");
    assert!(screen.contains("◇ (no role)"), "{screen}");
    assert!(!screen.contains("working"), "{screen}");

    let now = std::time::Instant::now();
    let ids = tick(&mut state, now);
    assert_eq!(ids.len(), 1, "one pull on open");
    assert!(tick(&mut state, now).is_empty(), "one in flight");
    reply(&mut state, &ids[0], Some(detailed_team()));
    let screen = text(&mut state);
    for expected in [
        "set by user",
        "◆ fixer",
        "claude",
        "pane_2",
        "working",
        "4m",
        "◇ (no role)",
        "codex",
        "idle",
        "12m",
        "✎ set a role",
        "removed: w2:p5",
        "+ join",
        "click a member to focus it",
    ] {
        assert!(
            screen.contains(expected),
            "missing {expected:?} in\n{screen}"
        );
    }
}

#[test]
fn it_re_pulls_after_a_push_and_on_the_minute_tick() {
    let mut state = team_state(Some("aim"));
    open_overlay(&mut state);
    let start = std::time::Instant::now();
    let ids = tick(&mut state, start);
    reply(&mut state, &ids[0], Some(detailed_team()));
    assert!(tick(&mut state, start).is_empty(), "nothing due");

    // A push for the group: pull on the next tick; the reply's status stays
    // on screen meanwhile.
    let mut changed = detailed_team();
    changed.purpose = Some("ship it".into());
    for member in &mut changed.members {
        member.status = None;
    }
    state.receive_teams(
        &ClientEndpointId::Local,
        payload("boot-1", 9, vec![changed]),
    );
    assert!(text(&mut state).contains("working"));
    let ids = tick(&mut state, start);
    assert_eq!(ids.len(), 1, "a push re-pulls");
    reply(&mut state, &ids[0], Some(detailed_team()));

    // The minute tick.
    assert!(tick(&mut state, start + std::time::Duration::from_secs(30)).is_empty());
    let ids = tick(&mut state, start + std::time::Duration::from_secs(61));
    assert_eq!(ids.len(), 1, "periodic pull");

    // A reply for another group (an overlay opened since) is dropped.
    let mut other = detailed_team();
    other.workspace_id = "ws_9".into();
    other.purpose = Some("other".into());
    state.handle_team_endpoint_result(
        super::super::teams::TeamRequestKind::Get {
            workspace_id: "ws_9".into(),
        },
        Ok(ResponseResult::TeamReply {
            team: Some(other),
            renamed: None,
        }),
    );
    // (The purpose line: the rights footer says "each other".)
    let shown = text(&mut state);
    assert!(shown.contains("purpose   fix calendar sync"), "{shown}");
    assert!(!shown.contains("purpose   other"), "{shown}");

    // The team disbanded: the overlay says so.
    state.receive_teams(&ClientEndpointId::Local, payload("boot-1", 10, Vec::new()));
    assert!(overlay(&state).gone);
    reply(&mut state, &ids[0], None);
    assert!(text(&mut state).contains("this group is not a team any more"));
}

#[test]
fn edit_opens_rename_and_the_overlay_comes_back() {
    use super::super::team_overlay::TeamOverlayHit;
    let mut state = team_state(Some("aim"));
    open_overlay(&mut state);
    text(&mut state);
    // ✎ on a member: the role prompt, then Team info again after saving.
    let edit = hit(
        &state,
        &TeamOverlayHit::EditRole {
            pane_id: "pane_3".into(),
        },
    );
    click(&mut state, edit);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::TeamRole { ref pane_id, reopen: true, .. },
            ..
        })) if pane_id == "pane_3"
    ));
    let outcome = state.handle_input_bytes(b"reviewer\r");
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamSetRole(params))] = &requests[..] else {
        panic!("team.set_role, got {requests:?}");
    };
    assert_eq!(
        (params.pane_id.as_str(), params.role.as_deref()),
        ("pane_3", Some("reviewer"))
    );
    assert_eq!(overlay(&state).workspace_id, "ws_2", "Team info reopened");

    // ✎ on the purpose, cancelled with Esc: back to Team info, nothing sent.
    text(&mut state);
    click_on(&mut state, &TeamOverlayHit::EditPurpose);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::TeamPurpose {
                make: false,
                reopen: true,
                ..
            },
            ..
        }))
    ));
    let outcome = state.handle_input_bytes(b"\x1b");
    assert!(endpoint_requests(&outcome).is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::TeamInfo(_))
    ));

    // A click outside the Rename modal also cancels back to Team info.
    text(&mut state);
    click_on(&mut state, &TeamOverlayHit::EditPurpose);
    text(&mut state);
    click(&mut state, Rect::new(0, 0, 1, 1));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::TeamInfo(_))
    ));
}

#[test]
fn remove_join_and_row_clicks_send_the_right_methods() {
    use super::super::team_overlay::TeamOverlayHit;
    let mut state = team_state(Some("aim"));
    open_overlay(&mut state);
    text(&mut state);
    let outcome = click_on(
        &mut state,
        &TeamOverlayHit::Remove {
            pane_id: "pane_2".into(),
        },
    );
    let requests = endpoint_requests(&outcome);
    let [(id, Method::TeamLeave(params))] = &requests[..] else {
        panic!("team.leave, got {requests:?}");
    };
    assert_eq!(params.pane_id, "pane_2");
    // Its reply makes the overlay pull again.
    let id = id.clone();
    reply(&mut state, &id, Some(team(Some("aim"))));
    assert!(overlay(&state).refresh_due);

    let outcome = click_on(
        &mut state,
        &TeamOverlayHit::Join {
            pane_id: "w2:p5".into(),
        },
    );
    let requests = endpoint_requests(&outcome);
    let [(_, Method::TeamJoin(params))] = &requests[..] else {
        panic!("team.join, got {requests:?}");
    };
    assert_eq!((params.pane_id.as_str(), &params.role), ("w2:p5", &None));

    // A click on a member row (not a button) focuses its pane and closes.
    let row = state
        .hits
        .team_overlay
        .iter()
        .find(|(_, hit)| {
            *hit == TeamOverlayHit::Focus {
                pane_id: "pane_3".into(),
            }
        })
        .map(|(rect, _)| *rect)
        .expect("row");
    let outcome = click(&mut state, Rect::new(row.x + 2, row.y, 1, 1));
    let requests = endpoint_requests(&outcome);
    assert!(
        requests.iter().any(|(_, method)| matches!(
            method,
            Method::PaneFocus(target) if target.pane_id == "pane_3"
        )),
        "{requests:?}"
    );
    assert!(state.overlay.is_none());
}

#[test]
fn keys_move_act_and_close() {
    let mut state = team_state(Some("aim"));
    open_overlay(&mut state);
    text(&mut state);
    // Purpose, fixer, (no role), removed.
    assert_eq!(overlay(&state).rows().len(), 4);
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(overlay(&state).cursor, 1);
    let outcome = state.handle_input_bytes(b"x");
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::TeamLeave(params))] if params.pane_id == "pane_2"),
        "{requests:?}"
    );
    state.handle_input_bytes(b"\x1b[B\x1b[B");
    let outcome = state.handle_input_bytes(b"\r");
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::TeamJoin(params))] if params.pane_id == "w2:p5"),
        "{requests:?}"
    );
    state.handle_input_bytes(b"\x1b");
    assert!(state.overlay.is_none());
}

#[test]
fn a_server_without_team_get_is_never_pulled() {
    let mut state = team_state(Some("aim"));
    state.set_endpoint_methods(Some(vec!["tab.focus".into()]));
    open_overlay(&mut state);
    assert!(tick(&mut state, std::time::Instant::now()).is_empty());
    assert!(!overlay(&state).loading);
}
