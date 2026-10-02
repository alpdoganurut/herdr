//! The settings overlay's browser section (fork): rows, keys, the requests
//! they send, the facts block, polling while the server checks or fixes,
//! and the unsupported-server fallback.

use super::*;
use crate::api::schema::{
    BrowserCheckInfo, BrowserFixKind, BrowserFixResult, BrowserSettingsInfo, Method, ResponseResult,
};
use crate::client::shell::settings_browser::{
    install_command, next_color, next_mcp_agents, row_labels, COLOR_PRESETS, ROW_COLOR, ROW_FIX,
    ROW_HIDE_NATIVE, ROW_INSTALL, ROW_MCP, ROW_OPEN_STOP, ROW_STEER,
};
use crate::config::{Config, SidebarLayoutConfig};

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config
}

fn tabs_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

/// The section's requests; the sidebar's own `browser.get` pulls (tested in
/// browser.rs) are left out.
fn endpoint_requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
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

fn settings_frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(110, 34).expect("settings frame");
    frame_rows(&frame).join("\n")
}

/// Tab through the sections to `browser`; returns the last press's outcome.
fn open_browser_section(state: &mut ClientShellState) -> ClientShellInput {
    state.open_settings_overlay();
    let index = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::Browser)
        .expect("browser section");
    let mut outcome = ClientShellInput::default();
    for _ in 0..index {
        outcome = state.handle_input_bytes(b"\t");
    }
    outcome
}

fn check(id: &str, ok: bool, detail: &str, fix_kind: BrowserFixKind) -> BrowserCheckInfo {
    BrowserCheckInfo {
        id: id.into(),
        ok,
        detail: detail.into(),
        fixable: fix_kind != BrowserFixKind::None,
        fix_kind,
    }
}

fn info() -> BrowserSettingsInfo {
    BrowserSettingsInfo {
        enabled: true,
        show_activity: true,
        pin_dashboard: true,
        activity_color: "#aa6eff".into(),
        steer_agents: true,
        wrap_agents: true,
        disable_native_browser: true,
        mcp_agents: vec!["claude".into()],
        shell_hook: true,
        profile: "main".into(),
        running: true,
        status: "running · 3 tabs · 2 agents · profile main".into(),
        checks: vec![
            check(
                "executable",
                true,
                "~/Applications/herdr+ Browser.app (home (branded))",
                BrowserFixKind::None,
            ),
            check(
                "helper",
                true,
                "playwright-core 1.63.0 · node v22.14.0",
                BrowserFixKind::Safe,
            ),
            check("extension", true, "v10 · ready", BrowserFixKind::None),
            check(
                "mcp_claude",
                true,
                "claude: registered",
                BrowserFixKind::EditsFiles,
            ),
            check(
                "mcp_codex",
                false,
                "codex: not registered",
                BrowserFixKind::EditsFiles,
            ),
            check("launch_context", true, "Aqua", BrowserFixKind::None),
        ],
        checked_at: Some(1_800_000_000),
        checking: false,
        fixing: false,
        fixes: Vec::new(),
        host: "mac".into(),
    }
}

fn reply(state: &mut ClientShellState, request_id: &str, settings: BrowserSettingsInfo) -> bool {
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::BrowserSettings { settings }),
    );
    repaint
}

fn one_set(outcome: &ClientShellInput) -> (String, String, serde_json::Value) {
    let requests = endpoint_requests(outcome);
    match &requests[..] {
        [(id, Method::BrowserSettingsSet(params))] => {
            (id.clone(), params.key.clone(), params.value.clone())
        }
        other => panic!("expected one browser.settings.set, got {other:?}"),
    }
}

fn selected_row(state: &ClientShellState) -> usize {
    match &state.overlay {
        Some(ClientShellOverlay::Settings(settings)) => settings.selected,
        _ => panic!("settings overlay"),
    }
}

#[test]
fn the_browser_section_pulls_the_record_and_shows_rows_and_facts() {
    let mut state = tabs_state();
    state.compose(110, 34).expect("composed frame");
    let outcome = open_browser_section(&mut state);
    let requests = endpoint_requests(&outcome);
    let [(request_id, Method::BrowserSettings(_))] = &requests[..] else {
        panic!("entering the section pulls browser.settings, got {requests:?}");
    };
    let text = settings_frame_text(&mut state);
    assert!(text.contains("loading browser settings"), "{text}");
    assert!(
        !text.contains("↵ apply"),
        "no primary button before the record"
    );

    assert!(reply(&mut state, request_id, info()));
    let text = settings_frame_text(&mut state);
    for expected in [
        "browser: on",
        "show activity (groups, glow, cursor): on",
        "pinned dashboard: on",
        "activity colour  #aa6eff ■",
        "agents prefer herdr's browser (when wrapped): on",
        "hide agents' own browsers (when wrapped): on",
        "MCP for: claude ✓  codex ✗",
        "▸ fix all (1 issue)",
        "stop browser",
        "install herdr+ Browser from a Chromium build… (↵ copies the command)",
        "status",
        "running · 3 tabs · 2 agents · profile main",
        "✓ ~/Applications/herdr+ Browser.app (home (branded))",
        "✓ playwright-core 1.63.0 · node v22.14.0",
        "✓ v10 · ready",
        "claude ✓ · codex ✗ not registered",
        "↵ apply",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    // the shell hook moved to Settings → Agents: no row, no fact
    assert!(!text.contains("shell hook"), "{text}");
    // the tab strip holds every section including the new one
    assert!(text.contains(" browser "), "{text}");
    assert_eq!(state.browser_section_rows(), 10);
}

#[test]
fn rows_send_the_settings_writes_and_the_actions() {
    let mut state = tabs_state();
    state.compose(110, 34).expect("composed frame");
    let outcome = open_browser_section(&mut state);
    let [(request_id, _)] = &endpoint_requests(&outcome)[..] else {
        panic!("one pull");
    };
    reply(&mut state, request_id, info());

    // row 0: browser on -> off through the server
    let (id, key, value) = one_set(&state.handle_input_bytes(b"\r"));
    assert_eq!(
        (key.as_str(), value),
        ("enabled", serde_json::Value::Bool(false))
    );
    let mut off = info();
    off.enabled = false;
    reply(&mut state, &id, off);
    let text = settings_frame_text(&mut state);
    assert!(
        text.contains("browser: off"),
        "the reply refreshes the row: {text}"
    );

    // the colour row: → cycles the presets (Right is not a section switch here)
    for _ in 0..ROW_COLOR {
        state.handle_input_bytes(b"\x1b[B");
    }
    assert_eq!(selected_row(&state), ROW_COLOR);
    let (id, key, value) = one_set(&state.handle_input_bytes(b"\x1b[C"));
    assert_eq!(key, "activity_color");
    assert_eq!(value, serde_json::Value::String(COLOR_PRESETS[1].into()));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Browser,
            ..
        }))
    ));
    let mut cyan = info();
    cyan.activity_color = COLOR_PRESETS[1].into();
    reply(&mut state, &id, cyan);
    assert!(settings_frame_text(&mut state).contains("activity colour  #00c8ff ■"));
    // a custom colour cycles to the first preset
    assert_eq!(next_color("#123456"), COLOR_PRESETS[0]);
    assert_eq!(next_color(COLOR_PRESETS[4]), COLOR_PRESETS[0]);

    // the steering row writes `steer_agents` only (the wrap is Settings → Agents)
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(selected_row(&state), ROW_STEER);
    let (id, key, value) = one_set(&state.handle_input_bytes(b"\r"));
    assert_eq!(
        (key.as_str(), value),
        ("steer_agents", serde_json::Value::Bool(false))
    );
    let mut steer_off = info();
    steer_off.steer_agents = false;
    reply(&mut state, &id, steer_off);
    assert!(settings_frame_text(&mut state)
        .contains("agents prefer herdr's browser (when wrapped): off"));
    // the hide row writes its own key
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(selected_row(&state), ROW_HIDE_NATIVE);
    let (id, key, value) = one_set(&state.handle_input_bytes(b"\r"));
    assert_eq!(
        (key.as_str(), value),
        ("disable_native_browser", serde_json::Value::Bool(false))
    );
    reply(&mut state, &id, info());
    let mut outcome = ClientShellInput::default();
    state.tick_browser(std::time::Instant::now(), &mut outcome);
    assert!(
        endpoint_requests(&outcome).is_empty(),
        "nothing more to write"
    );

    // the MCP row: → cycles claude -> codex
    for _ in ROW_HIDE_NATIVE..ROW_MCP {
        state.handle_input_bytes(b"\x1b[B");
    }
    let (id, key, value) = one_set(&state.handle_input_bytes(b"\x1b[C"));
    assert_eq!(key, "mcp_agents");
    assert_eq!(value, serde_json::json!(["codex"]));
    assert_eq!(next_mcp_agents(&["codex".into()]), Vec::<String>::new());
    assert_eq!(
        next_mcp_agents(&[]),
        vec!["claude".to_string(), "codex".to_string()]
    );
    let mut codex = info();
    codex.mcp_agents = vec!["codex".into()];
    codex.fixing = true;
    reply(&mut state, &id, codex);
    let text = settings_frame_text(&mut state);
    assert!(text.contains("MCP for: claude ✗  codex ✓"), "{text}");
    assert!(text.contains("▸ fixing…"), "{text}");
    assert!(
        state.next_browser_settings_deadline().is_some(),
        "the server is fixing: the section polls"
    );
    let deadline = state.next_browser_settings_deadline().unwrap();
    let mut outcome = ClientShellInput::default();
    state.tick_browser(deadline, &mut outcome);
    let requests = endpoint_requests(&outcome);
    let [(id, Method::BrowserSettings(_))] = &requests[..] else {
        panic!("the poll pulls browser.settings, got {requests:?}");
    };
    reply(&mut state, id, info());

    // fix all: browser.fix with no ids
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(selected_row(&state), ROW_FIX);
    let outcome = state.handle_input_bytes(b"\r");
    let requests = endpoint_requests(&outcome);
    let [(id, Method::BrowserFix(params))] = &requests[..] else {
        panic!("expected browser.fix, got {requests:?}");
    };
    assert!(params.ids.is_empty());
    let mut fixed = info();
    fixed.fixes = vec![BrowserFixResult {
        id: "mcp_codex".into(),
        ok: true,
        detail: "codex: registered".into(),
    }];
    fixed.checks.iter_mut().for_each(|c| c.ok = true);
    reply(&mut state, id, fixed);
    let text = settings_frame_text(&mut state);
    assert!(text.contains("last fix"), "{text}");
    assert!(text.contains("mcp_codex ✓"), "{text}");
    // nothing to fix: Enter sends nothing
    assert!(text.contains("▸ fix all (nothing to fix)"), "{text}");
    assert!(endpoint_requests(&state.handle_input_bytes(b"\r")).is_empty());

    // open · stop: running -> browser.stop, stopped -> browser.start
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(selected_row(&state), ROW_OPEN_STOP);
    let outcome = state.handle_input_bytes(b"\r");
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::BrowserStop(params))] if params.profile.as_deref() == Some("main")),
        "{requests:?}"
    );
    let mut stopped = info();
    stopped.running = false;
    stopped.status = "stopped · profile main".into();
    // the stop reply is a browser.get; the section's own pull follows
    let (id, _) = &requests[0];
    state.handle_endpoint_result(
        "boot-1",
        id,
        Ok(ResponseResult::BrowserGet {
            browser: crate::api::schema::BrowserGetInfo::default(),
        }),
    );
    let mut outcome = ClientShellInput::default();
    state.tick_browser(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
        &mut outcome,
    );
    let requests = endpoint_requests(&outcome);
    let [(id, Method::BrowserSettings(_))] = &requests[..] else {
        panic!("the section pulls again after the stop, got {requests:?}");
    };
    reply(&mut state, id, stopped);
    let text = settings_frame_text(&mut state);
    assert!(text.contains("open browser"), "{text}");
    assert!(text.contains("stopped · profile main"), "{text}");
    let outcome = state.handle_input_bytes(b"\r");
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::BrowserStart(params))] if params.profile.as_deref() == Some("main")),
        "{requests:?}"
    );
    let (id, _) = &requests[0];
    state.handle_endpoint_result(
        "boot-1",
        id,
        Ok(ResponseResult::BrowserGet {
            browser: crate::api::schema::BrowserGetInfo::default(),
        }),
    );

    // install: the command goes to the clipboard, the note says so
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(selected_row(&state), ROW_INSTALL);
    let outcome = state.handle_input_bytes(b"\r");
    assert!(
        outcome.actions.iter().any(|action| matches!(
            action,
            ClientShellAction::ClipboardWrite(bytes) if bytes == install_command().as_bytes()
        )),
        "{:?}",
        outcome.actions
    );
    assert!(endpoint_requests(&outcome).is_empty());
    let text = settings_frame_text(&mut state);
    assert!(
        text.contains("copied `herdr browser install-chromium <Chromium.app>`"),
        "{text}"
    );
}

#[test]
fn polling_while_the_server_checks_and_after_a_refusal() {
    let mut state = tabs_state();
    state.compose(110, 34).expect("composed frame");
    let outcome = open_browser_section(&mut state);
    let [(request_id, _)] = &endpoint_requests(&outcome)[..] else {
        panic!("one pull");
    };
    let mut checking = info();
    checking.checks.clear();
    checking.checking = true;
    reply(&mut state, request_id, checking);
    let text = settings_frame_text(&mut state);
    assert!(text.contains("checking…"), "{text}");
    assert!(text.contains("▸ fix all (nothing to fix)"), "{text}");
    let deadline = state
        .next_browser_settings_deadline()
        .expect("a poll is scheduled while the server checks");
    let mut outcome = ClientShellInput::default();
    state.tick_browser(deadline - std::time::Duration::from_millis(1), &mut outcome);
    assert!(
        endpoint_requests(&outcome).is_empty(),
        "not before the deadline"
    );
    state.tick_browser(deadline, &mut outcome);
    let requests = endpoint_requests(&outcome);
    let [(id, Method::BrowserSettings(_))] = &requests[..] else {
        panic!("the poll pulls browser.settings, got {requests:?}");
    };
    // endpoint_busy: keep what is shown, try again shortly
    state.handle_endpoint_result(
        "boot-1",
        id,
        Err(ClientShellEndpointError {
            code: Some("endpoint_busy".into()),
            message: "busy".into(),
        }),
    );
    assert!(state.next_browser_settings_deadline().is_some());
    let text = settings_frame_text(&mut state);
    assert!(text.contains("browser: on"), "{text}");
    assert!(
        !text.contains("not applied"),
        "busy is not an error: {text}"
    );
    // a refused write says why
    let deadline = state.next_browser_settings_deadline().unwrap();
    let mut outcome = ClientShellInput::default();
    state.tick_browser(deadline, &mut outcome);
    let [(id, _)] = &endpoint_requests(&outcome)[..] else {
        panic!("one pull");
    };
    state.handle_endpoint_result(
        "boot-1",
        id,
        Err(ClientShellEndpointError {
            code: Some("browser_config_write_failed".into()),
            message: "failed to save browser setting: read-only file system".into(),
        }),
    );
    let text = settings_frame_text(&mut state);
    assert!(
        text.contains("not applied: failed to save browser setting"),
        "{text}"
    );
    // a settled reply stops the polling
    let deadline = state.next_browser_settings_deadline().unwrap();
    let mut outcome = ClientShellInput::default();
    state.tick_browser(deadline, &mut outcome);
    let [(id, _)] = &endpoint_requests(&outcome)[..] else {
        panic!("one pull");
    };
    reply(&mut state, id, info());
    assert!(state.next_browser_settings_deadline().is_none());
    // leaving the section stops it too
    state.handle_input_bytes(b"\x1b");
    assert!(state.overlay.is_none());
    assert!(state.next_browser_settings_deadline().is_none());
}

#[test]
fn a_server_without_browser_settings_says_so_and_sends_nothing() {
    let mut state = tabs_state();
    state.set_endpoint_methods(Some(vec!["tab.focus".into(), "browser.get".into()]));
    state.compose(110, 34).expect("composed frame");
    let outcome = open_browser_section(&mut state);
    assert!(
        endpoint_requests(&outcome).is_empty(),
        "{:?}",
        endpoint_requests(&outcome)
    );
    let text = settings_frame_text(&mut state);
    assert!(
        text.contains("browser settings unavailable on this server (an older herdr)"),
        "{text}"
    );
    assert!(!text.contains("↵ apply"));
    assert_eq!(state.browser_section_rows(), 0);
    assert!(endpoint_requests(&state.handle_input_bytes(b"\r")).is_empty());
}

#[test]
fn remote_rows_say_where_the_edits_land_and_right_still_switches_sections() {
    let mut labels = row_labels(&info(), Some("Build"));
    assert_eq!(labels[ROW_MCP], "MCP for: claude ✓  codex ✗ (on Build)");
    assert_eq!(labels[ROW_FIX], "▸ fix all (1 issue) (on Build)");
    assert_eq!(labels.len(), 10);
    assert!(
        !labels.iter().any(|l| l.contains("shell hook")),
        "{labels:?}"
    );
    assert!(labels[ROW_INSTALL].ends_with("(on Build)"));
    assert_eq!(labels[0], "browser: on", "toggles carry no suffix");
    labels = row_labels(&info(), None);
    assert!(!labels.iter().any(|l| l.contains("(on ")));

    // Right on a toggle row is the overlay's section switch, not a cycle
    let mut state = tabs_state();
    state.compose(110, 34).expect("composed frame");
    let outcome = open_browser_section(&mut state);
    let [(request_id, _)] = &endpoint_requests(&outcome)[..] else {
        panic!("one pull");
    };
    reply(&mut state, request_id, info());
    let outcome = state.handle_input_bytes(b"\x1b[C");
    assert!(endpoint_requests(&outcome).is_empty());
    assert!(
        matches!(
            state.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Coordinator,
                ..
            }))
        ),
        "right moves on to the next section"
    );
}

#[test]
fn the_browser_row_hints_at_setup_needed() {
    use crate::client::shell::browser::{browser_row_state, BrowserRowState};
    let mut info = crate::api::schema::BrowserGetInfo {
        enabled: true,
        ..Default::default()
    };
    assert_eq!(
        browser_row_state(&info),
        (BrowserRowState::Stopped, "stopped".into())
    );
    info.setup_needed = true;
    assert_eq!(
        browser_row_state(&info),
        (BrowserRowState::Stopped, "stopped !".into())
    );
    info.profiles.push(crate::api::schema::BrowserProfileInfo {
        name: "main".into(),
        state: "running".into(),
        tabs: 2,
        agents: 1,
        ..Default::default()
    });
    assert_eq!(
        browser_row_state(&info),
        (BrowserRowState::Running, "2 tabs · 1 agent !".into())
    );
    info.profiles[0].state = "crashed".into();
    assert_eq!(
        browser_row_state(&info).1,
        "crashed",
        "a crash outranks the hint"
    );
}

#[test]
fn an_unchanged_browser_get_reply_still_carries_the_setup_hint() {
    let mut state = tabs_state();
    state.compose(110, 34).expect("composed frame");
    let mut outcome = ClientShellInput::default();
    state.tick_browser(std::time::Instant::now(), &mut outcome);
    let requests: Vec<_> = outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.clone()),
            _ => None,
        })
        .collect();
    let [request] = &requests[..] else {
        panic!("one browser.get, got {requests:?}");
    };
    let full = crate::api::schema::BrowserGetInfo {
        seq: 3,
        enabled: true,
        ..Default::default()
    };
    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(ResponseResult::BrowserGet { browser: full }),
    );
    assert_eq!(
        state.browser_row().map(|row| row.status),
        Some("stopped".to_string())
    );
    state.refresh_browser();
    let mut outcome = ClientShellInput::default();
    state.tick_browser(
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        &mut outcome,
    );
    let request = outcome
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.clone()),
            _ => None,
        })
        .expect("a second pull");
    let unchanged = crate::api::schema::BrowserGetInfo {
        seq: 3,
        unchanged: true,
        enabled: true,
        setup_needed: true,
        ..Default::default()
    };
    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(ResponseResult::BrowserGet { browser: unchanged }),
    );
    assert_eq!(
        state.browser_row().map(|row| row.status),
        Some("stopped !".to_string()),
        "the hint arrives with an unchanged reply"
    );
}
