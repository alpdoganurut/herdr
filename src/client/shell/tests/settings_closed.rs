use super::*;
use crate::api::schema::{ClosedSessionInfo, Method, ResponseResult};

fn settings_frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("settings frame");
    frame_rows(&frame).join("\n")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn entry(id: &str, label: Option<&str>, group: &str, cwd: &str, age: u64) -> ClosedSessionInfo {
    ClosedSessionInfo {
        id: id.into(),
        source: "herdr:claude".into(),
        agent: "claude".into(),
        session_ref_kind: crate::agent_resume::AgentSessionRefKind::Id,
        session_id: format!("session-{id}"),
        transcript_path: None,
        label: label.map(str::to_string),
        color: None,
        important: false,
        remind_every: None,
        pinned: false,
        muted: false,
        space_id: format!("w_{group}"),
        space_name: group.into(),
        cwd: cwd.into(),
        closed_at: now() - age,
    }
}

fn sessions() -> Vec<ClosedSessionInfo> {
    vec![
        entry("a", Some("level plan"), "leap", "/src/leap-bi-4", 5 * 60),
        entry("b", None, "herdr", "/src/herdr", 2 * 3_600),
        entry("c", Some("docs"), "leap", "/src/website", 3 * 86_400),
    ]
}

fn endpoint_requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
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

/// Tab through the sections to `closed`; returns the list request's id.
fn open_closed_section(state: &mut ClientShellState) -> Option<String> {
    state.open_settings_overlay();
    let index = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::ClosedSessions)
        .expect("closed section");
    let mut outcome = ClientShellInput::default();
    for _ in 0..index {
        outcome = state.handle_input_bytes(b"\t");
    }
    match &endpoint_requests(&outcome)[..] {
        [(id, Method::SessionClosedList(_))] => Some(id.clone()),
        [] => None,
        other => panic!("unexpected requests {other:?}"),
    }
}

fn state_with_sessions() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let request_id = open_closed_section(&mut state).expect("the section asks for the list");
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(ResponseResult::SessionClosedList {
            sessions: sessions(),
        }),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    state
}

fn closed_settings(state: &ClientShellState) -> &ClientSettingsOverlay {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Settings(settings))
            if settings.section == ClientSettingsSection::ClosedSessions =>
        {
            settings
        }
        other => panic!("closed section not open: {other:?}"),
    }
}

fn visible_ids(state: &ClientShellState) -> Vec<String> {
    crate::client::shell::settings_closed::filtered_closed_sessions(closed_settings(state))
        .into_iter()
        .map(|entry| entry.id.clone())
        .collect()
}

#[test]
fn closed_section_fetches_the_list_and_renders_rows_newest_first() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let request_id = open_closed_section(&mut state).expect("list requested");
    assert!(closed_settings(&state).closed.loading);
    let text = settings_frame_text(&mut state);
    assert!(text.contains("recently closed"), "{text}");
    assert!(text.contains("loading closed sessions"), "{text}");

    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(ResponseResult::SessionClosedList {
            sessions: sessions(),
        }),
    );
    assert!(!closed_settings(&state).closed.loading);
    let text = settings_frame_text(&mut state);
    let first = text
        .find("level plan · leap · leap-bi-4 · 5m ago")
        .expect(&text);
    let second = text.find("claude · herdr · herdr · 2h ago").expect(&text);
    let third = text.find("docs · leap · website · 3d ago").expect(&text);
    assert!(first < second && second < third, "newest first");
    assert!(text.contains("▸ level plan"), "first row selected");
    assert!(text.contains(" ↵ reopen "), "{text}");
    assert!(text.contains("esc close"));
}

#[test]
fn closed_section_shows_empty_and_unavailable_states() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let request_id = open_closed_section(&mut state).unwrap();
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(ResponseResult::SessionClosedList {
            sessions: Vec::new(),
        }),
    );
    let text = settings_frame_text(&mut state);
    assert!(text.contains("no closed agent sessions"), "{text}");
    assert!(!text.contains(" ↵ reopen "), "nothing to reopen");

    // An older server without the method: unavailable, not wedged.
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(Vec::new()));
    assert!(open_closed_section(&mut state).is_none());
    assert!(!closed_settings(&state).closed.loading);
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .is_some_and(|notice| notice.key.code == "session.closed_list"));
    let text = settings_frame_text(&mut state);
    assert!(text.contains("closed sessions unavailable"), "{text}");
    // Enter and Delete do nothing without a list.
    assert!(endpoint_requests(&state.handle_input_bytes(b"\r")).is_empty());
    assert!(endpoint_requests(&state.handle_input_bytes(b"\x1b[3~")).is_empty());
}

#[test]
fn typing_filters_on_label_group_and_dir_and_esc_clears_before_closing() {
    let mut state = state_with_sessions();

    // Letters the overlay otherwise uses for navigation go to the filter.
    state.handle_input_bytes(b"LEAP");
    assert_eq!(closed_settings(&state).closed.filter, "LEAP");
    assert_eq!(visible_ids(&state), ["a", "c"], "group, case-insensitive");
    let text = settings_frame_text(&mut state);
    assert!(text.contains("recently closed · filter: LEAP"), "{text}");
    assert!(!text.contains("claude · herdr"), "{text}");

    state.handle_input_bytes(b"\x7f\x7f\x7f\x7f");
    assert_eq!(closed_settings(&state).closed.filter, "");
    state.handle_input_bytes(b"websi");
    assert_eq!(visible_ids(&state), ["c"], "directory");
    state.handle_input_bytes(b"\x7f\x7f\x7f\x7f\x7f");
    state.handle_input_bytes(b"level");
    assert_eq!(visible_ids(&state), ["a"], "label");
    state.handle_input_bytes(b"x");
    assert!(visible_ids(&state).is_empty());
    let text = settings_frame_text(&mut state);
    assert!(text.contains("nothing matches the filter"), "{text}");
    assert!(endpoint_requests(&state.handle_input_bytes(b"\r")).is_empty());

    // Esc clears the filter first, and closes the overlay only after.
    state.handle_input_bytes(b"\x1b");
    assert_eq!(closed_settings(&state).closed.filter, "");
    assert_eq!(visible_ids(&state), ["a", "b", "c"]);
    state.handle_input_bytes(b"\x1b");
    assert!(state.overlay.is_none());
}

#[test]
fn enter_reopens_the_selected_filtered_row_and_closes_the_overlay() {
    let mut state = state_with_sessions();
    state.handle_input_bytes(b"leap");
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(closed_settings(&state).selected, 1);
    let text = settings_frame_text(&mut state);
    assert!(text.contains("▸ docs"), "{text}");
    // Down stops at the last filtered row.
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(closed_settings(&state).selected, 1);

    let outcome = state.handle_input_bytes(b"\r");
    match &endpoint_requests(&outcome)[..] {
        [(_, Method::SessionClosedReopen(target))] => assert_eq!(target.id, "c"),
        other => panic!("expected a reopen request, got {other:?}"),
    }
    assert!(state.overlay.is_none());
}

#[test]
fn delete_and_d_remove_the_entry_and_refresh_the_list() {
    let mut state = state_with_sessions();
    state.handle_input_bytes(b"\x1b[B");
    let outcome = state.handle_input_bytes(b"\x1b[3~");
    let [(remove_id, Method::SessionClosedRemove(target))] = &endpoint_requests(&outcome)[..]
    else {
        panic!("expected a remove request");
    };
    assert_eq!(target.id, "b");
    assert_eq!(visible_ids(&state), ["a", "c"], "gone at once");
    assert_eq!(closed_settings(&state).selected, 1);

    let (repaint, actions) =
        state.handle_endpoint_result("boot-1", remove_id, Ok(ResponseResult::Ok {}));
    assert!(repaint);
    assert!(
        matches!(
            &actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(request.method, Method::SessionClosedList(_))
        ),
        "the list is fetched again"
    );

    // `d` removes while the filter is empty, and types otherwise.
    let outcome = state.handle_input_bytes(b"d");
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::SessionClosedRemove(target))] if target.id == "c"
    ));
    state.handle_input_bytes(b"o");
    state.handle_input_bytes(b"d");
    assert_eq!(closed_settings(&state).closed.filter, "od");
    assert!(state.overlay.is_some());
}

#[test]
fn clicking_a_row_selects_it_without_reopening() {
    let mut state = state_with_sessions();
    settings_frame_text(&mut state);
    let (rect, index) = state.hits.settings_choices[2];
    assert_eq!(index, 2);
    let mut outcome = ClientShellInput::default();
    state.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 3,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut outcome,
    );
    assert!(endpoint_requests(&outcome).is_empty());
    assert_eq!(closed_settings(&state).selected, 2);
    assert!(state.overlay.is_some());
}
