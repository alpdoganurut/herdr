//! Fork (force suspend): the refusal notice's force button
//! (`force_notice.rs`).

use super::*;
use crate::api::schema::{AgentRestartParams, AgentSuspendParams, Method};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

fn press(column: u16, row: u16) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })
}

fn sent(outcome: &ClientShellInput) -> Vec<&Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .collect()
}

/// Send `method` and answer it with the refusal `code`.
fn refuse(state: &mut ClientShellState, method: Method, code: &str) {
    let mut outcome = ClientShellInput::default();
    state.push_endpoint_method(method, &mut outcome);
    let request_id = outcome
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.id.clone()),
            _ => None,
        })
        .expect("request sent");
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some(code.into()),
            message: format!("refused: {code}"),
        }),
    );
}

fn suspend(target: &str) -> Method {
    Method::AgentSuspend(AgentSuspendParams {
        target: target.into(),
    })
}

fn restart(target: &str) -> Method {
    Method::AgentRestart(AgentRestartParams {
        target: target.into(),
    })
}

#[test]
fn a_refusal_force_overrides_gets_the_button_and_others_do_not() {
    for code in super::super::force_notice::FORCEABLE_CODES {
        for restarting in [false, true] {
            let mut state = state();
            let method = if restarting {
                restart("pane_1")
            } else {
                suspend("pane_1")
            };
            refuse(&mut state, method, code);
            let notice = state.visible_endpoint_notice.as_ref().expect("notice");
            assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Rejected);
            assert_eq!(notice.body, format!("refused: {code}"));
            let force = notice.force.as_ref().expect("force button");
            let expected = if restarting {
                ("Force restart", "agent.restart_force")
            } else {
                ("Force suspend", "agent.suspend_force")
            };
            assert_eq!(force.label, expected.0);
            assert_eq!(crate::api::api_method_name(&force.method), expected.1);
            assert!(
                notice.deadline >= std::time::Instant::now() + std::time::Duration::from_secs(7),
                "long enough to aim at"
            );
        }
    }
    for code in [
        "agent_not_suspendable",
        "agent_not_ready",
        "agent_already_suspended",
        "agent_not_found",
    ] {
        let mut state = state();
        refuse(&mut state, suspend("pane_1"), code);
        let notice = state.visible_endpoint_notice.as_ref().expect("notice");
        assert!(notice.force.is_none(), "{code} cannot be forced");
    }
    // Another method's refusal with the same code: no button.
    let mut state = state();
    refuse(
        &mut state,
        Method::AgentActivate(crate::api::schema::AgentActivateParams {
            target: "pane_1".into(),
        }),
        "agent_working",
    );
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .unwrap()
        .force
        .is_none());
}

#[test]
fn a_server_without_the_force_method_keeps_the_plain_notice() {
    let mut state = state();
    let methods: std::collections::HashSet<String> = ["agent.suspend", "agent.restart"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let active = state.active_endpoint_id.clone();
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == active)
        .expect("active endpoint")
        .methods = Some(methods);
    refuse(&mut state, suspend("pane_1"), "agent_working");
    let notice = state.visible_endpoint_notice.as_ref().expect("notice");
    assert!(notice.force.is_none());
}

#[test]
fn the_force_button_sends_the_forced_method_and_closes_the_notice() {
    let mut state = state();
    refuse(&mut state, suspend("pane_1"), "agent_blocked");
    state.compose(120, 30).expect("composed frame");
    let card = state.hits.notification_toast;
    let button = state.hits.notification_toast_action;
    assert!(!button.is_empty(), "the button is drawn");
    assert!(
        button.x >= card.x
            && button.right() <= card.right()
            && button.y > card.y
            && button.bottom() < card.bottom(),
        "inside the card: {button:?} in {card:?}"
    );
    let frame = state.compose(120, 30).expect("composed frame");
    let row: String = frame_rows(&frame)[button.y as usize]
        .chars()
        .skip(button.x as usize)
        .take(button.width as usize)
        .collect();
    assert_eq!(row.trim(), "Force suspend");

    // A press on the card's title row only dismisses, as before.
    let outcome = state.handle_raw_events(vec![press(card.x + 2, card.y + 1)]);
    assert!(sent(&outcome).is_empty());
    assert!(state.visible_endpoint_notice.is_none());

    refuse(&mut state, suspend("pane_1"), "agent_blocked");
    state.compose(120, 30).expect("composed frame");
    let button = state.hits.notification_toast_action;
    let outcome = state.handle_raw_events(vec![press(button.x + 1, button.y)]);
    assert!(matches!(
        sent(&outcome).as_slice(),
        [Method::AgentSuspendForce(params)] if params.target == "pane_1"
    ));
    assert!(state.visible_endpoint_notice.is_none());
    state.compose(120, 30).expect("composed frame");
    assert!(state.hits.notification_toast_action.is_empty());
}

#[test]
fn a_notice_without_a_button_has_no_button_hit() {
    let mut state = state();
    refuse(&mut state, suspend("pane_1"), "agent_not_suspendable");
    state.compose(120, 30).expect("composed frame");
    assert!(!state.hits.notification_toast.is_empty());
    assert!(state.hits.notification_toast_action.is_empty());
}
