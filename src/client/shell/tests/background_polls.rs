//! Fork: the tick's own requests when an endpoint switch or a lost connection
//! cancels them: no notice, asked again (the tick's reads `POLL_RETRY` later,
//! the overlays' pulls on the next tick that may send).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::coordinator::CoordinatorRequestKind;
use super::super::info_dock_model::InfoDockTarget;
use super::super::machine_tabs::POLL_RETRY;
use super::super::teams::TeamRequest;
use super::info_dock::{dock_state, editor_text, error, notes_info, sent_save, start_dirty_edit};
use super::*;
use crate::api::schema::notes::{NotesWriteInfo, NotesWriteOutcome};
use crate::api::schema::{EmptyParams, Method, NewsGetInfo, ResponseResult, TabTarget};
use crate::client::endpoint::{EndpointNegotiation, EndpointRegistry, EndpointTransport};
use crate::client::endpoint_commands::EndpointCommands;

fn state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

/// The client's tick, as far as it polls (the order of `client/mod.rs`).
fn tick(state: &mut ClientShellState) -> ClientShellInput {
    tick_at(state, Instant::now())
}

fn tick_at(state: &mut ClientShellState, now: Instant) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_news(now, &mut outcome);
    state.tick_perf_readout(now, &mut outcome);
    state.tick_coordinator(now, &mut outcome);
    state.tick_teams(now, &mut outcome);
    state.tick_browser(now, &mut outcome);
    state.tick_info_dock(now, &mut outcome);
    outcome
}

fn requests(outcome: &ClientShellInput) -> Vec<(String, &'static str)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some((
                request.id.clone(),
                crate::api::api_method_name(&request.method),
            )),
            _ => None,
        })
        .collect()
}

fn methods(outcome: &ClientShellInput) -> Vec<&'static str> {
    requests(outcome)
        .into_iter()
        .map(|(_, method)| method)
        .collect()
}

fn news_info() -> NewsGetInfo {
    NewsGetInfo {
        enabled: true,
        times: vec!["08:00".into()],
        quiet_hours: "00:00-08:00".into(),
        model: None,
        tab_id: None,
        pane_id: None,
        next_run_at: None,
        run: None,
        last_run: None,
        unread: false,
        consecutive_failures: 0,
        pending_notifications: 0,
        last_read_edition: None,
        new_stories: None,
    }
}

fn notice_title(state: &ClientShellState) -> Option<&str> {
    state
        .visible_endpoint_notice
        .as_ref()
        .map(|notice| notice.title.as_str())
}

const TICK_POLLS: [&str; 3] = ["news.get", "coordinator.get", "browser.get"];

#[test]
fn cancelled_tick_polls_raise_no_notice_keep_the_rows_and_are_sent_again() {
    let mut state = state();
    let first = tick(&mut state);
    assert_eq!(methods(&first), TICK_POLLS);
    for (id, method) in requests(&first) {
        let result = match method {
            "news.get" => ResponseResult::NewsGet { news: news_info() },
            "coordinator.get" => ResponseResult::CoordinatorGet {
                info: Default::default(),
            },
            _ => ResponseResult::BrowserGet {
                browser: Default::default(),
            },
        };
        state.handle_endpoint_result("boot-1", &id, Ok(result));
    }
    assert!(tick(&mut state).actions.is_empty(), "nothing due");

    state.refresh_news();
    state.refresh_browser();
    state.coordinator.refresh();
    // Cancelled each time it asks until the fence opens.
    let mut now = Instant::now();
    for _ in 0..3 {
        let polls = tick_at(&mut state, now);
        assert_eq!(methods(&polls), TICK_POLLS);
        for (id, method) in requests(&polls) {
            assert!(
                !state.cancel_endpoint_request(&id),
                "{method}: a cancelled poll repaints nothing"
            );
        }
        assert_eq!(notice_title(&state), None);
        assert!(tick(&mut state).actions.is_empty(), "not every tick");
        now = Instant::now() + POLL_RETRY;
        assert!(state.pending_requests.is_empty());
        assert!(state.news.info.is_some());
        assert!(state.browser.info.is_some());
        assert!(state.coordinator.info.is_some());
    }
}

#[test]
fn a_poll_waits_for_an_offline_endpoint_instead_of_counting_as_done() {
    let mut state = state();
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
    for _ in 0..2 {
        assert!(tick(&mut state).actions.is_empty());
        assert_eq!(notice_title(&state), None);
        assert_eq!(
            state.next_browser_deadline(Instant::now()),
            None,
            "an offline endpoint's due pull must not spin the loop"
        );
    }
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    let polls = tick(&mut state);
    assert_eq!(methods(&polls), TICK_POLLS);

    // The same after a pull was answered: its next one falls due offline.
    let (id, _) = requests(&polls)
        .into_iter()
        .find(|(_, method)| *method == "browser.get")
        .expect("browser.get");
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(ResponseResult::BrowserGet {
            browser: Default::default(),
        }),
    );
    let due = state
        .next_browser_deadline(Instant::now())
        .expect("the next periodic pull");
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
    assert_eq!(state.next_browser_deadline(due), None);
}

#[test]
fn a_cancelled_team_pull_is_silent_sent_again_and_waits_while_offline() {
    let mut state = state();
    state.open_team_overlay("ws_1".into());
    for _ in 0..2 {
        let mut outcome = ClientShellInput::default();
        state.tick_teams(Instant::now(), &mut outcome);
        assert_eq!(methods(&outcome), ["team.get"]);
        assert!(!state.cancel_endpoint_request(&requests(&outcome)[0].0));
        assert_eq!(notice_title(&state), None);
    }
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
    assert!(tick(&mut state).actions.is_empty());
    assert_eq!(notice_title(&state), None);
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    let mut outcome = ClientShellInput::default();
    state.tick_teams(Instant::now(), &mut outcome);
    assert_eq!(methods(&outcome), ["team.get"]);
}

#[test]
fn a_team_overlay_opened_offline_pulls_once_the_endpoint_is_back() {
    let mut state = state();
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
    state.open_team_overlay("ws_1".into());
    for _ in 0..2 {
        state.tick_teams(Instant::now(), &mut ClientShellInput::default());
    }
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    let mut outcome = ClientShellInput::default();
    state.tick_teams(Instant::now(), &mut outcome);
    assert_eq!(methods(&outcome), ["team.get"]);
}

#[test]
fn a_cancelled_browser_settings_pull_is_silent_and_polled_again() {
    let mut state = state();
    let mut outcome = ClientShellInput::default();
    state.open_settings_overlay();
    state.select_settings_section(ClientSettingsSection::Browser, &mut outcome);
    let id = requests(&outcome)
        .into_iter()
        .find_map(|(id, method)| (method == "browser.settings").then_some(id))
        .expect("the section pulls");
    assert!(!state.cancel_endpoint_request(&id));
    assert_eq!(notice_title(&state), None);
    let due = state
        .next_browser_settings_deadline()
        .expect("the section pulls again shortly");
    let mut outcome = ClientShellInput::default();
    state.tick_browser(due, &mut outcome);
    let id = requests(&outcome)
        .into_iter()
        .find_map(|(id, method)| (method == "browser.settings").then_some(id))
        .expect("pulled again");

    // Cancelled by a lost connection: due once the endpoint is back.
    assert!(!state.cancel_endpoint_request(&id));
    let due = state
        .next_browser_settings_deadline()
        .expect("the section pulls again shortly");
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
    let later = due + Duration::from_secs(1);
    let mut outcome = ClientShellInput::default();
    state.tick_browser(later, &mut outcome);
    assert_eq!(notice_title(&state), None);
    assert_eq!(state.next_browser_settings_deadline(), None);
    assert!(state.timer_delay(later) > Duration::ZERO);
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    let mut outcome = ClientShellInput::default();
    state.tick_browser(later, &mut outcome);
    assert!(methods(&outcome).contains(&"browser.settings"));
}

#[test]
fn a_cancelled_info_dock_pull_is_silent_and_sent_again_on_the_next_tick() {
    let mut state = state();
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane_for_focused_tab(&mut outcome);
    for _ in 0..2 {
        let mut polls = std::mem::take(&mut outcome);
        if polls.actions.is_empty() {
            state.tick_info_dock(Instant::now(), &mut polls);
        }
        let sent = requests(&polls);
        let [(id, "notes.get")] = &sent[..] else {
            panic!("expected one notes.get, got {sent:?}");
        };
        assert!(!state.cancel_endpoint_request(id));
        assert_eq!(notice_title(&state), None);
    }
}

#[test]
fn a_cancelled_notes_save_starts_no_request_and_leaves_the_dock_free() {
    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let outcome = state.handle_input_bytes(b"\x1b");
    let (save, _) = sent_save(&outcome).expect("save on Esc");
    // A task ticked from an old row asks for the notes behind the save.
    let mut outcome = ClientShellInput::default();
    state.info_dock_activate_for_test(
        InfoDockTarget::Task {
            line: 0,
            revision: "sha256:old".into(),
        },
        &mut outcome,
    );
    assert!(outcome.actions.is_empty(), "the save holds the slot");

    let (_, actions) =
        state.handle_endpoint_result("boot-1", &save, Err(error("endpoint_cancelled")));
    assert!(
        actions.is_empty(),
        "a cancellation starts no request: {actions:?}"
    );
    let editor = state
        .info_dock
        .as_deref()
        .and_then(|dock| dock.editor.as_ref())
        .expect("buffer kept");
    assert!(editor.dirty && !editor.save_pending && editor.sent_text.is_none());

    let mut outcome = ClientShellInput::default();
    state.tick_info_dock(Instant::now(), &mut outcome);
    let sent = requests(&outcome);
    let [(pull, "notes.get")] = &sent[..] else {
        panic!("the dock is stuck: {sent:?}");
    };
    state.handle_endpoint_result(
        "boot-1",
        pull,
        Ok(ResponseResult::NotesGet {
            notes: notes_info("notes", "sha256:4e9b1c07a2f3"),
        }),
    );
    let mut outcome = ClientShellInput::default();
    state.tick_info_dock(Instant::now() + Duration::from_secs(3), &mut outcome);
    assert!(sent_save(&outcome).is_some(), "the save is sent again");
}

#[test]
fn a_cancelled_conflict_text_pull_is_sent_again_while_the_editor_is_open() {
    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let outcome = state.handle_input_bytes(b"\x1b");
    let (save, _) = sent_save(&outcome).expect("save on Esc");
    let theirs = notes_info("their text", "sha256:theirs");
    let mut without_text = theirs.clone();
    without_text.text = None;
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &save,
        Ok(ResponseResult::NotesWrite {
            write: NotesWriteInfo {
                outcome: NotesWriteOutcome::Conflict,
                notes: without_text,
            },
        }),
    );
    let mut outcome = ClientShellInput {
        actions,
        ..Default::default()
    };
    // Cancelled on every tick until the fence opens.
    for _ in 0..3 {
        let sent = requests(&outcome);
        let [(pull, "notes.get")] = &sent[..] else {
            panic!("expected one notes.get, got {sent:?}");
        };
        assert!(!state.cancel_endpoint_request(pull));
        outcome = ClientShellInput::default();
        state.tick_info_dock(Instant::now(), &mut outcome);
    }
    let sent = requests(&outcome);
    let [(pull, "notes.get")] = &sent[..] else {
        panic!("expected one notes.get, got {sent:?}");
    };
    state.handle_endpoint_result(
        "boot-1",
        pull,
        Ok(ResponseResult::NotesGet { notes: theirs }),
    );
    state.info_dock_activate_for_test(
        InfoDockTarget::ConflictReload,
        &mut ClientShellInput::default(),
    );
    assert_eq!(editor_text(&state), "their text");
}

#[test]
fn a_users_cancelled_action_still_raises_the_notice() {
    let user_actions: [fn(&mut ClientShellState, &mut ClientShellInput); 5] = [
        |state, outcome| {
            state.push_endpoint_method(
                Method::TabFocus(TabTarget {
                    tab_id: "tab_1".into(),
                }),
                outcome,
            );
        },
        |state, outcome| {
            state.push_endpoint_method_with_kind(
                Method::NewsOpen(Default::default()),
                PendingEndpointKind::NewsOpen,
                outcome,
            );
        },
        |state, outcome| {
            state.push_endpoint_method_with_kind(
                Method::BrowserStop(Default::default()),
                PendingEndpointKind::BrowserStop,
                outcome,
            );
        },
        |state, outcome| {
            state.push_endpoint_method_with_kind(
                Method::CoordinatorWake(Default::default()),
                PendingEndpointKind::Coordinator(CoordinatorRequestKind::Wake),
                outcome,
            );
        },
        |state, outcome| {
            state.push_team_request(
                TeamRequest::Disband {
                    workspace_id: "ws_1".into(),
                },
                outcome,
            );
        },
    ];
    for action in user_actions {
        let mut state = state();
        let mut outcome = ClientShellInput::default();
        action(&mut state, &mut outcome);
        let sent = requests(&outcome);
        let [(id, method)] = &sent[..] else {
            panic!("expected one request, got {sent:?}");
        };
        state.cancel_endpoint_request(id);
        assert_eq!(notice_title(&state), Some("Action interrupted"), "{method}");
    }
}

#[test]
fn a_polls_other_errors_keep_their_notice() {
    let mut state = state();
    let polls = tick(&mut state);
    let (id, _) = requests(&polls).swap_remove(0);
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Err(ClientShellEndpointError {
            code: Some("server_unavailable".into()),
            message: "the server is unavailable".into(),
        }),
    );
    assert_eq!(notice_title(&state), Some("Server unavailable"));
}

struct Capture(Arc<Mutex<Vec<ClientMessage>>>);

impl EndpointTransport for Capture {
    fn send(&mut self, message: &ClientMessage) -> std::io::Result<()> {
        self.0.lock().unwrap().push(message.clone());
        Ok(())
    }
}

/// The switch as the client loop runs it: the target's coherent frame is
/// shown, its fence still blocks sends, and the tick polls the new server.
#[test]
fn fork_smoke_polls_before_the_targets_fence_raise_no_notice_and_follow_it() {
    let mut state = state();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut endpoints =
        EndpointRegistry::new(Capture(sent.clone()), 1, EndpointNegotiation::default());
    endpoints.set_surface_active(&ClientEndpointId::Local, true);
    endpoints.freeze_input();
    assert!(!endpoints.active_surface_available());
    let mut commands = EndpointCommands::default();
    let dispatch = |state: &mut ClientShellState,
                    endpoints: &mut EndpointRegistry,
                    commands: &mut EndpointCommands,
                    actions: Vec<ClientShellAction>| {
        crate::client::shell_runtime::dispatch_client_shell_actions(
            actions,
            commands,
            endpoints,
            Some(state),
            &mut Vec::new(),
            &mut None,
        )
        .unwrap()
        .1
    };

    let mut now = Instant::now();
    for _ in 0..3 {
        let polls = tick_at(&mut state, now);
        assert_eq!(methods(&polls), TICK_POLLS);
        let repaint = dispatch(&mut state, &mut endpoints, &mut commands, polls.actions);
        assert!(!repaint, "a blocked poll composes no frame");
        assert_eq!(notice_title(&state), None);
        now = Instant::now() + POLL_RETRY;
    }
    assert!(sent.lock().unwrap().is_empty(), "the fence blocks sends");

    // The fence opens: the next polls go out.
    endpoints.unfreeze_input();
    let polls = tick_at(&mut state, now);
    assert_eq!(methods(&polls), TICK_POLLS);
    dispatch(&mut state, &mut endpoints, &mut commands, polls.actions);
    assert_eq!(notice_title(&state), None);
    let first = sent.lock().unwrap().clone();
    assert!(
        matches!(&first[..], [ClientMessage::ClientShellEndpointRequest { request, .. }]
            if request.contains("news.get")),
        "{first:?}"
    );

    // The user's own action in that window still says so.
    endpoints.freeze_input();
    let mut action = ClientShellInput::default();
    state.push_endpoint_method(Method::CoordinatorOpen(EmptyParams::default()), &mut action);
    assert!(dispatch(
        &mut state,
        &mut endpoints,
        &mut commands,
        action.actions
    ));
    assert_eq!(notice_title(&state), Some("Action interrupted"));
}
