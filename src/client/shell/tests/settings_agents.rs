//! The settings overlay's agents section (fork): rows, dimmed sub-rows,
//! the requests they send, and the armed `[fix]` that sends `agents.fix`
//! only after an explicit confirm. The client never edits a file: these
//! tests only look at the outgoing requests.

use super::*;
use crate::api::schema::{
    AgentsCheckInfo, AgentsCheckState, AgentsFixParams, AgentsSettingsInfo, AgentsWrapSource,
    Method, ResponseResult,
};
use crate::client::shell::settings_agents::{
    row_labels, ClientAgentsSettings, HIT_CANCEL, HIT_CONFIRM, ROWS, ROW_FILE, ROW_INSTRUCTIONS,
    ROW_STATUS, ROW_TOOLS,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn agents_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

fn requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. }
                if !matches!(request.method, Method::BrowserGet(_)) =>
            {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

fn frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(120, 40).expect("settings frame");
    frame_rows(&frame).join("\n")
}

/// Tab through the sections to `agents` (the last one).
fn open_agents_section(state: &mut ClientShellState) -> ClientShellInput {
    state.open_settings_overlay();
    let index = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::Agents)
        .expect("agents section");
    assert_eq!(
        index,
        ClientSettingsSection::ALL.len() - 1,
        "agents is last"
    );
    let mut outcome = ClientShellInput::default();
    for _ in 0..index {
        outcome = state.handle_input_bytes(b"\t");
    }
    outcome
}

fn hook(state: AgentsCheckState) -> AgentsCheckInfo {
    AgentsCheckInfo {
        id: "shell_hook".into(),
        state,
        detail: match state {
            AgentsCheckState::Ok => "~/.zshrc → herdr-plus.zsh (v2)".into(),
            _ => "no herdr+ line in ~/.zshrc".into(),
        },
        fixable: state != AgentsCheckState::Ok,
        edits_files: true,
    }
}

fn info(wrap: bool, hook_state: AgentsCheckState) -> AgentsSettingsInfo {
    AgentsSettingsInfo {
        wrap,
        wrap_source: AgentsWrapSource::BrowserLegacy,
        tools: false,
        instructions: false,
        instructions_file: String::new(),
        instructions_detail: "built-in".into(),
        steer_browser: true,
        notices: true,
        checks: vec![
            hook(hook_state),
            AgentsCheckInfo {
                id: "claude".into(),
                state: AgentsCheckState::Ok,
                detail: "on the server's PATH (claude-z)".into(),
                fixable: false,
                edits_files: false,
            },
            AgentsCheckInfo {
                id: "codex".into(),
                state: AgentsCheckState::Absent,
                detail: "not on the server's PATH".into(),
                fixable: false,
                edits_files: false,
            },
        ],
        hook_preview: Some(
            "[ -f ~/.config/herdr/shell/herdr-plus.zsh ] && source ~/.config/herdr/shell/herdr-plus.zsh  # herdr+\n→ /home/me/.zshrc"
                .into(),
        ),
    }
}

fn reply(state: &mut ClientShellState, request_id: &str, info: AgentsSettingsInfo) {
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::AgentsSettings { info }),
    );
    assert!(repaint);
}

fn entered(state: &mut ClientShellState, info: AgentsSettingsInfo) {
    let outcome = open_agents_section(state);
    let requests = requests(&outcome);
    let [(id, Method::AgentsSettings(_))] = &requests[..] else {
        panic!("entering pulls agents.settings, got {requests:?}");
    };
    reply(state, id, info);
}

fn down_to(state: &mut ClientShellState, row: usize) {
    for _ in 0..row {
        state.handle_input_bytes(b"\x1b[B");
    }
    match &state.overlay {
        Some(ClientShellOverlay::Settings(settings)) => assert_eq!(settings.selected, row),
        _ => panic!("settings overlay"),
    }
}

fn armed(state: &ClientShellState) -> bool {
    match &state.overlay {
        Some(ClientShellOverlay::Settings(settings)) => settings.agents.armed,
        _ => false,
    }
}

fn choice_rect(state: &ClientShellState, index: usize) -> Rect {
    state
        .hits
        .settings_choices
        .iter()
        .find(|(_, hit)| *hit == index)
        .map(|(rect, _)| *rect)
        .expect("choice hit")
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 1,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn fix_requests(outcome: &ClientShellInput) -> Vec<AgentsFixParams> {
    requests(outcome)
        .into_iter()
        .filter_map(|(_, method)| match method {
            Method::AgentsFix(params) => Some(params),
            _ => None,
        })
        .collect()
}

#[test]
fn the_section_shows_the_rows_checks_and_hints() {
    let mut state = agents_state();
    let outcome = open_agents_section(&mut state);
    let [(id, Method::AgentsSettings(_))] = &requests(&outcome)[..] else {
        panic!("one pull");
    };
    let text = frame_text(&mut state);
    assert!(text.contains("loading agents settings"), "{text}");
    assert!(text.contains(" agents "), "the tab is drawn: {text}");
    reply(
        &mut state,
        &id.clone(),
        info(false, AgentsCheckState::Missing),
    );
    let text = frame_text(&mut state);
    for expected in [
        "wrap claude / codex in herdr+ panes: off   (from [browser] wrap_agents)",
        "  add agent tools (notify): off",
        "  add herdr+ instructions to the system prompt: off",
        "  instructions file: built-in   (↵ uses agents.md in herdr's config dir)",
        "shell hook ✗ · claude ✓ · codex ✗      ▸ fix (edits ~/.zshrc)",
        "shell hook ✗ no herdr+ line in ~/.zshrc",
        "claude     ✓ on the server's PATH (claude-z)",
        "codex      ✗ not on the server's PATH",
        "changes apply on the next launch; running agents keep what they launched with",
        "per launch: claude --no-herdr · codex --no-herdr · HERDR_NO_WRAP=1 · command claude",
        "browser steering: [browser] steer_agents = on (applies while wrapped)",
        "↵ apply",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    assert_eq!(state.agents_section_rows(), ROWS);
}

#[test]
fn the_sub_rows_are_dimmed_and_inert_while_the_wrap_is_off() {
    let mut state = agents_state();
    entered(&mut state, info(false, AgentsCheckState::Ok));
    let frame = state.compose(120, 40).expect("frame");
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let palette = state.config.palette.clone();
    let tools = choice_rect(&state, ROW_TOOLS);
    let file = choice_rect(&state, ROW_FILE);
    assert_eq!(
        buffer[(tools.x + 6, tools.y)].fg,
        palette.overlay0,
        "tools dimmed"
    );
    assert_eq!(
        buffer[(file.x + 6, file.y)].fg,
        palette.text,
        "file row is not"
    );

    for row in [ROW_TOOLS, ROW_INSTRUCTIONS] {
        let mut state = agents_state();
        entered(&mut state, info(false, AgentsCheckState::Ok));
        down_to(&mut state, row);
        let outcome = state.handle_input_bytes(b"\r");
        assert!(requests(&outcome).is_empty(), "row {row} inert while off");
    }

    // wrap on: they write their keys
    let mut state = agents_state();
    entered(&mut state, info(true, AgentsCheckState::Ok));
    down_to(&mut state, ROW_TOOLS);
    let outcome = state.handle_input_bytes(b"\r");
    let [(_, Method::AgentsSettingsSet(params))] = &requests(&outcome)[..] else {
        panic!("tools set");
    };
    assert_eq!(
        (params.key.as_str(), &params.value),
        ("tools", &serde_json::Value::Bool(true))
    );
}

#[test]
fn rows_send_their_settings_writes() {
    let mut state = agents_state();
    entered(&mut state, info(false, AgentsCheckState::Ok));
    let outcome = state.handle_input_bytes(b"\r");
    let requests_now = requests(&outcome);
    let [(id, Method::AgentsSettingsSet(params))] = &requests_now[..] else {
        panic!("wrap set, got {requests_now:?}");
    };
    assert_eq!(
        (params.key.as_str(), &params.value),
        ("wrap", &serde_json::Value::Bool(true))
    );
    let mut on = info(true, AgentsCheckState::Ok);
    on.wrap_source = AgentsWrapSource::Agents;
    reply(&mut state, id, on);
    let text = frame_text(&mut state);
    assert!(
        text.contains("wrap claude / codex in herdr+ panes: on"),
        "{text}"
    );
    assert!(!text.contains("(from [browser] wrap_agents)"), "{text}");

    down_to(&mut state, ROW_INSTRUCTIONS);
    let outcome = state.handle_input_bytes(b"\r");
    let [(id, Method::AgentsSettingsSet(params))] = &requests(&outcome)[..] else {
        panic!("instructions set");
    };
    assert_eq!(params.key, "instructions");
    let id = id.clone();
    reply(&mut state, &id, info(true, AgentsCheckState::Ok));

    // the file row: built-in -> "file", a file -> "default"
    state.handle_input_bytes(b"\x1b[B");
    let outcome = state.handle_input_bytes(b"\r");
    let [(id, Method::AgentsSettingsSet(params))] = &requests(&outcome)[..] else {
        panic!("instructions_file set");
    };
    assert_eq!(
        (params.key.as_str(), &params.value),
        (
            "instructions_file",
            &serde_json::Value::String("file".into())
        )
    );
    let mut file = info(true, AgentsCheckState::Ok);
    file.instructions_file = "/home/me/.config/herdr/agents.md".into();
    file.instructions_detail = "~/.config/herdr/agents.md (412 B)".into();
    let id = id.clone();
    reply(&mut state, &id, file);
    let text = frame_text(&mut state);
    assert!(
        text.contains(
            "instructions file: ~/.config/herdr/agents.md (412 B)   (↵ back to built-in)"
        ),
        "{text}"
    );
    let outcome = state.handle_input_bytes(b"\r");
    let [(_, Method::AgentsSettingsSet(params))] = &requests(&outcome)[..] else {
        panic!("instructions_file set");
    };
    assert_eq!(params.value, serde_json::Value::String("default".into()));
    match &state.overlay {
        Some(ClientShellOverlay::Settings(settings)) => assert_eq!(settings.selected, ROW_FILE),
        _ => panic!("settings overlay"),
    }

    // a refusal is shown, then the pull brings the real state back
    let mut state = agents_state();
    entered(&mut state, info(false, AgentsCheckState::Ok));
    let outcome = state.handle_input_bytes(b"\r");
    let [(id, _)] = &requests(&outcome)[..] else {
        panic!("one set");
    };
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        id,
        Err(ClientShellEndpointError {
            code: Some("agents_config_write_failed".into()),
            message: "read-only file system".into(),
        }),
    );
    assert!(actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. } if matches!(request.method, Method::AgentsSettings(_))
    )));
    assert!(frame_text(&mut state).contains("not applied: read-only file system"));
}

#[test]
fn the_fix_arms_first_and_only_a_confirm_sends_agents_fix() {
    let mut state = agents_state();
    entered(&mut state, info(true, AgentsCheckState::Missing));
    down_to(&mut state, ROW_STATUS);

    // ↵ arms: nothing is sent, the preview and the buttons show
    let outcome = state.handle_input_bytes(b"\r");
    assert!(requests(&outcome).is_empty(), "{:?}", requests(&outcome));
    assert!(armed(&state));
    let text = frame_text(&mut state);
    for expected in [
        "this edits ~/.zshrc:",
        "source ~/.config/herdr/shell/herdr-plus.zsh",
        "→ /home/me/.zshrc",
        "↵ edit ~/.zshrc",
        "esc cancel",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    assert!(!text.contains("↵ apply"), "the armed fix owns ↵: {text}");

    // esc disarms (and keeps the overlay open)
    let outcome = state.handle_input_bytes(b"\x1b");
    assert!(fix_requests(&outcome).is_empty());
    assert!(!armed(&state));
    assert!(state.overlay.is_some());

    // arm again; another key disarms and moves on
    state.handle_input_bytes(b"\r");
    assert!(armed(&state));
    let outcome = state.handle_input_bytes(b"\x1b[A");
    assert!(fix_requests(&outcome).is_empty());
    assert!(!armed(&state));
    state.handle_input_bytes(b"\x1b[B");

    // arm, ↵ confirms: agents.fix { ids: [shell_hook], confirm: true }
    state.handle_input_bytes(b"\r");
    let outcome = state.handle_input_bytes(b"\r");
    let fixes = fix_requests(&outcome);
    assert_eq!(
        fixes,
        [AgentsFixParams {
            ids: vec!["shell_hook".into()],
            confirm: true
        }]
    );
    assert!(!armed(&state));
    let [(id, _)] = &requests(&outcome)[..] else {
        panic!("one fix");
    };
    assert!(frame_text(&mut state).contains("▸ fixing…"));
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        id,
        Ok(ResponseResult::AgentsFix {
            results: vec![hook(AgentsCheckState::Ok)],
        }),
    );
    assert!(actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. } if matches!(request.method, Method::AgentsSettings(_))
    )));
    let text = frame_text(&mut state);
    assert!(text.contains("shell hook fixed"), "{text}");
    assert!(text.contains("shell hook ✓ · claude ✓"), "{text}");
    assert!(!text.contains("▸ fix"), "{text}");
}

#[test]
fn the_fix_buttons_take_clicks_and_a_click_elsewhere_disarms() {
    let mut state = agents_state();
    entered(&mut state, info(true, AgentsCheckState::Outdated));
    state.compose(120, 40).expect("frame");

    // a click on the status row arms, sends nothing
    let status = choice_rect(&state, ROW_STATUS);
    let outcome = click(&mut state, status);
    assert!(requests(&outcome).is_empty());
    assert!(armed(&state));
    assert!(frame_text(&mut state).contains("shell hook ✗ outdated"));

    // cancel button
    let cancel = choice_rect(&state, HIT_CANCEL);
    let outcome = click(&mut state, cancel);
    assert!(requests(&outcome).is_empty());
    assert!(!armed(&state));

    // a click on another row disarms (and acts on that row)
    click(&mut state, status);
    assert!(armed(&state));
    state.compose(120, 40).expect("frame");
    let file = choice_rect(&state, ROW_FILE);
    let outcome = click(&mut state, file);
    assert!(fix_requests(&outcome).is_empty());
    assert!(!armed(&state));

    // leaving the section disarms
    let mut state = agents_state();
    entered(&mut state, info(true, AgentsCheckState::Outdated));
    state.compose(120, 40).expect("frame");
    let status = choice_rect(&state, ROW_STATUS);
    click(&mut state, status);
    assert!(armed(&state));
    state.handle_input_bytes(b"\x1b[Z");
    assert!(!armed(&state));

    // the confirm button sends the fix
    let mut state = agents_state();
    entered(&mut state, info(true, AgentsCheckState::Missing));
    state.compose(120, 40).expect("frame");
    let status = choice_rect(&state, ROW_STATUS);
    click(&mut state, status);
    state.compose(120, 40).expect("frame");
    let confirm = choice_rect(&state, HIT_CONFIRM);
    let outcome = click(&mut state, confirm);
    assert_eq!(
        fix_requests(&outcome),
        [AgentsFixParams {
            ids: vec!["shell_hook".into()],
            confirm: true
        }]
    );
}

#[test]
fn nothing_to_fix_and_unsupported_servers_send_nothing() {
    // a current hook: the status row does nothing
    let mut state = agents_state();
    entered(&mut state, info(true, AgentsCheckState::Ok));
    down_to(&mut state, ROW_STATUS);
    let outcome = state.handle_input_bytes(b"\r");
    assert!(requests(&outcome).is_empty());
    assert!(!armed(&state));

    // an older server
    let mut state = agents_state();
    state.set_endpoint_methods(Some(vec!["tab.focus".into()]));
    let outcome = open_agents_section(&mut state);
    assert!(requests(&outcome).is_empty());
    let text = frame_text(&mut state);
    assert!(
        text.contains("agents settings unavailable on this server (an older herdr)"),
        "{text}"
    );
    assert!(!text.contains("↵ apply"));
    assert_eq!(state.agents_section_rows(), 0);
}

#[test]
fn remote_fix_labels_say_which_machine() {
    let settings = ClientAgentsSettings {
        remote: true,
        endpoint_label: "Build".into(),
        ..Default::default()
    };
    let labels = row_labels(&settings, &info(true, AgentsCheckState::Missing));
    assert!(
        labels[ROW_STATUS].ends_with("▸ fix (edits ~/.zshrc on Build)"),
        "{labels:?}"
    );
    let local = ClientAgentsSettings::default();
    let labels = row_labels(&local, &info(true, AgentsCheckState::Ok));
    assert!(!labels[ROW_STATUS].contains("▸"), "{labels:?}");
    assert_eq!(labels.len(), ROWS);
}
