//! Idle reminders: the "Remind me" tab mark (`tab.set_remind`), its sidebar
//! marker and key, the client reminder engine, and the settings section.

use super::*;
use crate::config::{Config, SidebarLayoutConfig};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
use std::time::{Duration, Instant};

const MINUTE: Duration = Duration::from_secs(60);

fn tab(tab_id: &str, label: &str, focused: bool, status: AgentStatus) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: "ws_1".into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused,
        agent_status: status,
        color: None,
        remind: false,
    }
}

fn agent(pane_id: &str, tab_id: &str, status: AgentStatus, seq: u64) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: tab_id.into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: seq,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }
}

/// tab_1 "reviewer" focused and idle; tab_2 "planner" in the background with
/// its agent (pane_2) in `status` at state sequence 2, marked when `marked`.
/// The client only projects `Done` for a completion it watched, so states
/// built with `reminder_state` pass through `Working` (sequence 1) first.
fn waiting_snapshot(status: AgentStatus, marked: bool) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.workspaces[0].label = "backend".into();
    let mut planner = tab("tab_2", "planner", false, status);
    planner.remind = marked;
    snapshot.tabs = vec![tab("tab_1", "reviewer", true, AgentStatus::Idle), planner];
    snapshot.agents = vec![
        agent("pane_1", "tab_1", AgentStatus::Idle, 1),
        agent("pane_2", "tab_2", status, 2),
    ];
    snapshot
}

/// `waiting_snapshot` with pane_2 working at `seq`.
fn working_snapshot(marked: bool, seq: u64) -> ClientShellSnapshot {
    let mut snapshot = waiting_snapshot(AgentStatus::Working, marked);
    snapshot.agents[1].state_change_seq = seq;
    snapshot
}

fn reminder_config(minutes: u32, delivery: crate::config::ToastDelivery) -> ClientShellConfig {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = delivery;
    config.toast_delay_seconds = 0;
    config.toast_sticky = true;
    config.idle_reminder_minutes = minutes;
    config
}

fn reminder_state(minutes: u32, snapshot: ClientShellSnapshot) -> ClientShellState {
    let mut state = ClientShellState::new(reminder_config(
        minutes,
        crate::config::ToastDelivery::Herdr,
    ));
    watch_work_then(&mut state, snapshot);
    state
}

/// Show the client pane_2 working, then `snapshot`.
fn watch_work_then(state: &mut ClientShellState, snapshot: ClientShellSnapshot) {
    let marked = snapshot.tabs.iter().any(|tab| tab.remind);
    state.set_snapshot(Box::new(working_snapshot(marked, 1)));
    state.set_snapshot(Box::new(snapshot));
}

fn cards(state: &ClientShellState) -> Vec<(String, Option<String>, SemanticNotificationKind)> {
    state
        .visible_notifications
        .iter()
        .chain(state.queued_notifications.iter())
        .map(|card| {
            (
                card.event.title.clone(),
                card.event.pane_id.clone(),
                card.event.kind,
            )
        })
        .collect()
}

fn sounds(effects: &[ClientShellNotificationEffect]) -> Vec<crate::sound::Sound> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            ClientShellNotificationEffect::Sound { sound, .. } => Some(*sound),
            _ => None,
        })
        .collect()
}

#[test]
fn no_reminder_before_the_interval_then_one_at_it() {
    let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();

    let (effects, _) = state.tick_notifications(t0);
    assert!(effects.is_empty());
    let (effects, _) = state.tick_notifications(t0 + 9 * MINUTE);
    assert!(effects.is_empty());
    assert!(cards(&state).is_empty());
    assert_eq!(state.next_idle_reminder_deadline(), Some(t0 + 10 * MINUTE));

    let (effects, repaint) = state.tick_notifications(t0 + 10 * MINUTE);
    assert!(repaint);
    assert_eq!(sounds(&effects), [crate::sound::Sound::Done]);
    let card = state.visible_notifications.back().expect("reminder card");
    assert_eq!(card.event.title, "planner finished 10 min ago");
    // "<agent> · <directory>": pane_2 has no pane row, so the space label.
    assert_eq!(card.event.body.as_deref(), Some("claude · backend"));
    assert_eq!(card.event.kind, SemanticNotificationKind::Finished);
    assert_eq!(card.event.pane_id.as_deref(), Some("pane_2"));
    assert_eq!(card.event.tab_id.as_deref(), Some("tab_2"));
    assert_eq!(state.next_idle_reminder_deadline(), Some(t0 + 20 * MINUTE));
}

#[test]
fn second_reminder_replaces_the_first_card() {
    let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    state.tick_notifications(t0 + 10 * MINUTE);
    state.tick_notifications(t0 + 15 * MINUTE);
    assert_eq!(cards(&state).len(), 1);

    let (effects, _) = state.tick_notifications(t0 + 20 * MINUTE);
    assert_eq!(sounds(&effects), [crate::sound::Sound::Done]);
    assert_eq!(
        cards(&state),
        [(
            "planner finished 20 min ago".to_string(),
            Some("pane_2".to_string()),
            SemanticNotificationKind::Finished
        )],
        "one card for the tab"
    );
}

#[test]
fn reminder_replaces_the_original_finished_card_for_the_pane() {
    let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();
    state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::Finished,
            title: "claude finished".into(),
            body: None,
            sound: None,
            agent: Some("claude".into()),
            workspace_id: Some("ws_1".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        t0,
    );
    assert_eq!(cards(&state).len(), 1);
    state.tick_notifications(t0 + 10 * MINUTE);
    assert_eq!(
        cards(&state)
            .into_iter()
            .map(|(title, ..)| title)
            .collect::<Vec<_>>(),
        ["planner finished 10 min ago"]
    );
}

#[test]
fn blocked_tab_reminds_as_needing_attention_with_the_request_sound() {
    let mut state = reminder_state(5, waiting_snapshot(AgentStatus::Blocked, true));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    let (effects, _) = state.tick_notifications(t0 + 5 * MINUTE);
    assert_eq!(sounds(&effects), [crate::sound::Sound::Request]);
    assert_eq!(
        cards(&state),
        [(
            "planner still waiting".to_string(),
            Some("pane_2".to_string()),
            SemanticNotificationKind::NeedsAttention
        )]
    );
}

#[test]
fn focusing_the_tab_resets_the_clock() {
    let snapshot = waiting_snapshot(AgentStatus::Blocked, true);
    let mut state = reminder_state(10, snapshot.clone());
    let t0 = Instant::now();
    state.tick_notifications(t0);

    let mut focused = snapshot.clone();
    focused.focused_tab_id = Some("tab_2".into());
    focused.focused_pane_id = Some("pane_2".into());
    focused.tabs[0].focused = false;
    focused.tabs[1].focused = true;
    state.set_snapshot(Box::new(focused));
    state.tick_notifications(t0 + 5 * MINUTE);
    assert!(state.idle_reminders.is_empty());

    state.set_snapshot(Box::new(snapshot));
    state.tick_notifications(t0 + 6 * MINUTE);
    let (effects, _) = state.tick_notifications(t0 + 10 * MINUTE);
    assert!(effects.is_empty(), "the clock restarted when it lost focus");
    assert!(cards(&state).is_empty());
    let (effects, _) = state.tick_notifications(t0 + 16 * MINUTE);
    assert_eq!(sounds(&effects), [crate::sound::Sound::Request]);
    assert_eq!(cards(&state).len(), 1);
}

#[test]
fn a_status_change_resets_the_clock() {
    let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();
    state.tick_notifications(t0);

    // Working again, then done again: a new state episode.
    state.set_snapshot(Box::new(working_snapshot(true, 3)));
    state.tick_notifications(t0 + 4 * MINUTE);
    assert!(state.idle_reminders.is_empty());
    let mut done_again = waiting_snapshot(AgentStatus::Done, true);
    done_again.agents[1].state_change_seq = 4;
    state.set_snapshot(Box::new(done_again.clone()));
    state.tick_notifications(t0 + 5 * MINUTE);
    let (effects, _) = state.tick_notifications(t0 + 10 * MINUTE);
    assert!(effects.is_empty());

    // Working and done again between two ticks: the same status with a newer
    // sequence also restarts the clock.
    state.set_snapshot(Box::new(working_snapshot(true, 5)));
    done_again.agents[1].state_change_seq = 6;
    state.set_snapshot(Box::new(done_again));
    state.tick_notifications(t0 + 12 * MINUTE);
    let (effects, _) = state.tick_notifications(t0 + 15 * MINUTE);
    assert!(effects.is_empty());
    let (effects, _) = state.tick_notifications(t0 + 22 * MINUTE);
    assert_eq!(sounds(&effects).len(), 1);
    assert_eq!(cards(&state)[0].0, "planner finished 10 min ago");
}

#[test]
fn removing_the_mark_or_the_tab_resets_the_clock() {
    let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    state.set_snapshot(Box::new(waiting_snapshot(AgentStatus::Done, false)));
    state.tick_notifications(t0 + MINUTE);
    assert!(state.idle_reminders.is_empty());

    state.set_snapshot(Box::new(waiting_snapshot(AgentStatus::Done, true)));
    state.tick_notifications(t0 + 2 * MINUTE);
    let mut gone = waiting_snapshot(AgentStatus::Done, true);
    gone.tabs.truncate(1);
    gone.agents.truncate(1);
    state.set_snapshot(Box::new(gone));
    state.tick_notifications(t0 + 3 * MINUTE);
    assert!(state.idle_reminders.is_empty());
    let (effects, _) = state.tick_notifications(t0 + 30 * MINUTE);
    assert!(effects.is_empty());
}

#[test]
fn unmarked_and_idle_tabs_never_remind() {
    let t0 = Instant::now();
    let seen = |snapshot| {
        // Never watched working: the client projects the agent as seen (idle).
        let mut state =
            ClientShellState::new(reminder_config(10, crate::config::ToastDelivery::Herdr));
        state.set_snapshot(Box::new(snapshot));
        state
    };
    for mut state in [
        reminder_state(10, waiting_snapshot(AgentStatus::Done, false)),
        reminder_state(10, working_snapshot(true, 2)),
        seen(waiting_snapshot(AgentStatus::Idle, true)),
        seen(waiting_snapshot(AgentStatus::Done, true)),
    ] {
        for minutes in [0, 10, 20, 30] {
            let (effects, _) = state.tick_notifications(t0 + minutes * MINUTE);
            assert!(effects.is_empty());
        }
        assert!(cards(&state).is_empty());
        assert_eq!(state.next_idle_reminder_deadline(), None);
    }
}

#[test]
fn zero_minutes_turns_reminders_off() {
    let mut state = reminder_state(0, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();
    for minutes in [0, 10, 240, 600] {
        let (effects, _) = state.tick_notifications(t0 + minutes * MINUTE);
        assert!(effects.is_empty());
    }
    assert!(cards(&state).is_empty());
    assert_eq!(state.next_idle_reminder_deadline(), None);
}

#[test]
fn sounds_off_reminds_silently() {
    let mut config = reminder_config(10, crate::config::ToastDelivery::Herdr);
    config.sound_enabled = false;
    let mut state = ClientShellState::new(config);
    watch_work_then(&mut state, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    let (effects, _) = state.tick_notifications(t0 + 10 * MINUTE);
    assert!(sounds(&effects).is_empty());
    assert_eq!(cards(&state).len(), 1);
}

#[test]
fn terminal_and_system_delivery_emit_their_effects() {
    use crate::config::ToastDelivery;
    for delivery in [ToastDelivery::Terminal, ToastDelivery::System] {
        let mut state = ClientShellState::new(reminder_config(10, delivery));
        watch_work_then(&mut state, waiting_snapshot(AgentStatus::Done, true));
        let t0 = Instant::now();
        state.tick_notifications(t0);
        let (effects, _) = state.tick_notifications(t0 + 10 * MINUTE);
        let delivered =
            effects
                .iter()
                .filter_map(|effect| match (effect, delivery) {
                    (
                        ClientShellNotificationEffect::Terminal { title, body },
                        ToastDelivery::Terminal,
                    )
                    | (
                        ClientShellNotificationEffect::System { title, body },
                        ToastDelivery::System,
                    ) => Some((title.as_str(), body.as_deref())),
                    _ => None,
                })
                .collect::<Vec<_>>();
        assert_eq!(
            delivered,
            [("planner finished 10 min ago", Some("claude · backend"))],
            "{delivery:?}"
        );
        assert!(cards(&state).is_empty());
    }
}

#[test]
fn timer_wakes_for_the_next_reminder() {
    let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Done, true));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    // Due now: the loop timer fires at once instead of after its idle poll.
    assert_eq!(state.timer_delay(t0 + 10 * MINUTE), Duration::ZERO);
}

// --- mark, menu, key, marker ------------------------------------------------

fn tabs_state(snapshot: ClientShellSnapshot) -> ClientShellState {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state
}

fn open_tab_menu(state: &mut ClientShellState, row: usize) -> Vec<ClientContextMenuItem> {
    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[row];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("tab context menu"),
    }
}

fn endpoint_methods(outcome: &ClientShellInput) -> Vec<crate::api::schema::Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.method.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn tab_menu_toggles_the_mark_without_focusing_the_tab() {
    use crate::api::schema::{Method, TabSetRemindParams};
    for (marked, label) in [(false, "Remind me"), (true, "Stop reminding")] {
        let mut state = tabs_state(waiting_snapshot(AgentStatus::Done, marked));
        let items = open_tab_menu(&mut state, 1);
        let labels = items.iter().map(|item| item.label).collect::<Vec<_>>();
        let close = labels.iter().position(|label| *label == "Close").unwrap();
        assert_eq!(labels[close + 1], label, "{labels:?}");
        assert_eq!(
            items.last().map(|item| item.action),
            Some(ClientContextMenuAction::Color),
            "the swatch row still ends the menu"
        );

        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(close + 1, &mut outcome);
        assert_eq!(
            endpoint_methods(&outcome),
            [Method::TabSetRemind(TabSetRemindParams {
                tab_id: "tab_2".into(),
                remind: !marked,
            })],
            "no tab.focus: focusing would mark the agent seen"
        );
        assert!(state.overlay.is_none());
    }
}

#[test]
fn close_keeps_its_index_in_the_tab_menu() {
    let mut state = tabs_state(waiting_snapshot(AgentStatus::Done, false));
    let labels = open_tab_menu(&mut state, 0)
        .iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    // No agent-specific items on the idle claude tab besides suspend/restart;
    // Close sits where it did before the reminder item existed.
    let close = labels.iter().position(|label| *label == "Close").unwrap();
    assert_eq!(&labels[..2], ["New tab", "Rename"]);
    assert_eq!(labels[close + 1], "Remind me");
}

#[test]
fn toggle_tab_remind_binding_flips_the_focused_tab() {
    use crate::api::schema::{Method, TabSetRemindParams};
    use crate::input::{KeybindAction, KeybindMatch};
    let mut snapshot = waiting_snapshot(AgentStatus::Done, false);
    for expected in [true, false] {
        snapshot.tabs[0].remind = !expected;
        let mut state = tabs_state(snapshot.clone());
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            KeybindMatch::Action(KeybindAction::ToggleTabRemind),
            &mut outcome,
        );
        assert_eq!(
            endpoint_methods(&outcome),
            [Method::TabSetRemind(TabSetRemindParams {
                tab_id: "tab_1".into(),
                remind: expected,
            })]
        );
    }
    let bound: Config = toml::from_str("[keys]\ntoggle_tab_remind = \"alt+r\"").unwrap();
    assert!(!bound
        .live_keybinds_with_diagnostics()
        .map(|(keybinds, _)| keybinds.keybinds.toggle_tab_remind.bindings.is_empty())
        .unwrap_or(true));
    assert!(!Config::default().keys.toggle_tab_remind.has_values());
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
fn marked_rows_show_the_marker_before_the_agent_glyph() {
    let marker = super::super::tab_sidebar::TAB_REMIND_MARKER;
    let mut state = tabs_state(waiting_snapshot(AgentStatus::Done, true));
    let frame = state.compose(106, 20).expect("composed frame");
    let rows = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| row_text(&frame, *rect))
        .collect::<Vec<_>>();
    assert!(!rows[0].contains(marker), "unmarked: {rows:?}");
    let marked = rows[1].trim_end();
    assert!(marked.contains("planner"), "{rows:?}");
    assert!(
        marked.ends_with(&format!("{marker} \u{29C6}")),
        "marker sits just before the glyph: {marked:?}"
    );
    // Same row width as an unmarked row: the label gives way, nothing overflows.
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(rows[0].as_str()),
        unicode_width::UnicodeWidthStr::width(rows[1].as_str())
    );
    // Drawn monochrome in overlay0.
    let (rect, _) = state.hits.sidebar_tabs[1];
    let marker_cell = (rect.x..rect.right())
        .map(|x| &frame.cells[(rect.y * frame.width + x) as usize])
        .find(|cell| cell.symbol == marker)
        .expect("marker cell");
    assert_eq!(
        marker_cell.fg,
        crate::protocol::color_to_u32(state.config.palette.overlay0)
    );
}

// --- settings ----------------------------------------------------------------

fn open_reminders_section(state: &mut ClientShellState) {
    state.open_settings_overlay();
    let index = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::Reminders)
        .expect("reminders section");
    for _ in 0..index {
        state.handle_input_bytes(b"\t");
    }
}

fn settings_selection(state: &ClientShellState) -> (ClientSettingsSection, usize) {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Settings(settings)) => (settings.section, settings.selected),
        _ => panic!("settings overlay"),
    }
}

#[test]
fn reminders_section_lists_every_choice_and_checks_the_configured_one() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    open_reminders_section(&mut state);
    assert_eq!(
        settings_selection(&state),
        (ClientSettingsSection::Reminders, 2),
        "the default, 10 min"
    );
    let frame = state.compose(106, 30).expect("settings frame");
    let text = frame_rows(&frame).join("\n");
    for label in ["off", "5 min", "10 min ✓", "15 min", "30 min", "60 min"] {
        assert!(text.contains(label), "{label}: {text}");
    }
    assert_eq!(
        state.hits.settings_choices.len(),
        6,
        "every choice is drawn"
    );

    // A configured value outside the list shows first, as custom.
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.idle_reminder_minutes = 45;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    open_reminders_section(&mut state);
    assert_eq!(settings_selection(&state).1, 0);
    let frame = state.compose(106, 30).expect("settings frame");
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("custom: 45 min ✓"), "{text}");
    assert!(text.contains("60 min"), "{text}");
    assert_eq!(state.hits.settings_choices.len(), 7);
}

#[test]
fn applying_a_reminder_choice_writes_the_config_and_reloads_it() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "herdr-idle-reminders-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\nidle_reminder_minutes = 45\n").unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

    let mut config = ClientShellConfig::from_config(&Config::default());
    config.idle_reminder_minutes = 45;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    open_reminders_section(&mut state);
    // custom (45), off, 5, 10, 15: pick 15 min.
    for _ in 0..4 {
        state.handle_input_bytes(b"j");
    }
    let outcome = state.handle_input_bytes(b"\r");

    let written = std::fs::read_to_string(&path).unwrap();
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(&dir);

    let parsed: Config = toml::from_str(&written).unwrap();
    assert_eq!(parsed.ui.idle_reminder_minutes, 15, "{written}");
    assert_eq!(state.config.idle_reminder_minutes, 15, "live reload");
    assert!(endpoint_methods(&outcome)
        .iter()
        .any(|method| matches!(method, crate::api::schema::Method::ServerReloadConfig(_))));
    // The custom row is gone; the cursor stays on the applied value.
    assert_eq!(
        settings_selection(&state),
        (ClientSettingsSection::Reminders, 3)
    );
}

#[test]
fn reminder_body_names_the_agent_and_the_pane_directory() {
    let mut snapshot = waiting_snapshot(AgentStatus::Blocked, true);
    let mut pane = snapshot.panes[0].clone();
    pane.pane_id = "pane_2".into();
    pane.tab_id = "tab_2".into();
    pane.cwd = Some("/home/me/src/leap-bi-4/".into());
    snapshot.panes.push(pane);
    let mut state = reminder_state(10, snapshot);
    let t0 = Instant::now();
    state.tick_notifications(t0);
    state.tick_notifications(t0 + 10 * MINUTE);
    let card = state.visible_notifications.back().expect("reminder card");
    assert_eq!(card.event.title, "planner still waiting");
    assert_eq!(card.event.body.as_deref(), Some("claude · leap-bi-4"));
}

/// Fork smoke tests: FORK.md section 10 lists them by name and the sync gate
/// runs them with `-E 'test(fork_smoke)'`.
mod fork_smoke {
    use super::*;

    #[test]
    fn marked_done_tab_reminds_after_the_interval() {
        let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Done, true));
        let t0 = Instant::now();
        let (effects, _) = state.tick_notifications(t0);
        assert!(effects.is_empty());
        let (effects, _) = state.tick_notifications(t0 + 10 * MINUTE - Duration::from_secs(1));
        assert!(effects.is_empty());
        let (effects, repaint) = state.tick_notifications(t0 + 10 * MINUTE);
        assert!(repaint);
        assert_eq!(sounds(&effects), [crate::sound::Sound::Done]);
        assert_eq!(
            cards(&state),
            [(
                "planner finished 10 min ago".to_string(),
                Some("pane_2".to_string()),
                SemanticNotificationKind::Finished
            )]
        );
    }
}
