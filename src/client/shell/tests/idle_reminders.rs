//! Tab reminders: important and scheduled (`tab.set_reminder`), the tab
//! menu's important item and reminder selector, the sidebar markers and key,
//! the client reminder engine, the reminder sound and the settings sections.

use super::*;
use crate::api::schema::{TabRemindEvery, TabRemindInterval};
use crate::config::{Config, SidebarLayoutConfig};
use crate::sound::{ReminderBase, Sound};
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
        important: false,
        remind_every: None,
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
/// its agent (pane_2) in `status` at state sequence 2, important when
/// `important`. The client only projects `Done` for a completion it watched,
/// so states built with `reminder_state` pass through `Working` (sequence 1)
/// first.
fn waiting_snapshot(status: AgentStatus, important: bool) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.workspaces[0].label = "backend".into();
    let mut planner = tab("tab_2", "planner", false, status);
    planner.important = important;
    snapshot.tabs = vec![tab("tab_1", "reviewer", true, AgentStatus::Idle), planner];
    snapshot.agents = vec![
        agent("pane_1", "tab_1", AgentStatus::Idle, 1),
        agent("pane_2", "tab_2", status, 2),
    ];
    snapshot
}

/// `waiting_snapshot` with pane_2 working at `seq`.
fn working_snapshot(important: bool, seq: u64) -> ClientShellSnapshot {
    let mut snapshot = waiting_snapshot(AgentStatus::Working, important);
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
    let important = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == "tab_2")
        .is_some_and(|tab| tab.important);
    state.set_snapshot(Box::new(working_snapshot(important, 1)));
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
    assert_eq!(sounds(&effects), [Sound::Reminder(ReminderBase::Done)]);
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
    assert_eq!(sounds(&effects), [Sound::Reminder(ReminderBase::Done)]);
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
    assert_eq!(sounds(&effects), [Sound::Reminder(ReminderBase::Request)]);
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
    assert_eq!(sounds(&effects), [Sound::Reminder(ReminderBase::Request)]);
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

// --- menu, key, markers ----------------------------------------------------

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

fn key(code: crossterm::event::KeyCode) -> RawInputEvent {
    RawInputEvent::Key(crate::input::TerminalKey::new(code, KeyModifiers::empty()))
}

/// The `tab.set_reminder` requests an outcome sends.
fn reminders_sent(
    outcome: &ClientShellInput,
) -> Vec<(String, Option<bool>, Option<TabRemindEvery>)> {
    endpoint_methods(outcome)
        .into_iter()
        .filter_map(|method| match method {
            crate::api::schema::Method::TabSetReminder(params) => {
                Some((params.tab_id, params.important, params.every))
            }
            _ => None,
        })
        .collect()
}

fn no_tab_focus(outcome: &ClientShellInput) -> bool {
    !endpoint_methods(outcome)
        .iter()
        .any(|method| matches!(method, crate::api::schema::Method::TabFocus(_)))
}

fn menu_remind(state: &ClientShellState) -> (usize, ClientTabMenuRemind) {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { remind, .. },
            highlighted,
            ..
        })) => (*highlighted, *remind),
        _ => panic!("tab context menu"),
    }
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

/// `waiting_snapshot` (idle, not important) with tab_2 reminding `every`.
fn scheduled_snapshot(every: TabRemindInterval) -> ClientShellSnapshot {
    let mut snapshot = waiting_snapshot(AgentStatus::Idle, false);
    snapshot.tabs[1].remind_every = Some(every);
    snapshot
}

fn with_tab_2_focused(mut snapshot: ClientShellSnapshot) -> ClientShellSnapshot {
    snapshot.focused_tab_id = Some("tab_2".into());
    snapshot.focused_pane_id = Some("pane_2".into());
    snapshot.tabs[0].focused = false;
    snapshot.tabs[1].focused = true;
    snapshot
}

fn seen_state(snapshot: ClientShellSnapshot) -> ClientShellState {
    let mut state = ClientShellState::new(reminder_config(10, crate::config::ToastDelivery::Herdr));
    state.set_snapshot(Box::new(snapshot));
    state
}

fn scheduled_lit(state: &ClientShellState) -> Option<bool> {
    state
        .scheduled_reminders
        .get(&(ClientEndpointId::Local, "tab_2".to_string()))
        .map(|reminder| reminder.lit)
}

fn reminder_card() -> (String, Option<String>, SemanticNotificationKind) {
    (
        "planner reminder".to_string(),
        Some("pane_2".to_string()),
        SemanticNotificationKind::Custom,
    )
}

#[test]
fn interval_reminder_fires_on_schedule_whatever_the_agent_does() {
    let mut state = seen_state(scheduled_snapshot(TabRemindInterval::M5));
    let t0 = Instant::now();
    assert!(state.tick_notifications(t0).0.is_empty());
    assert!(state.tick_notifications(t0 + 4 * MINUTE).0.is_empty());
    assert_eq!(state.next_idle_reminder_deadline(), Some(t0 + 5 * MINUTE));
    let (effects, repaint) = state.tick_notifications(t0 + 5 * MINUTE);
    assert!(repaint);
    assert_eq!(sounds(&effects), [Sound::Reminder(ReminderBase::Done)]);
    assert_eq!(cards(&state), [reminder_card()]);
    let card = state.visible_notifications.back().unwrap();
    assert_eq!(card.event.body.as_deref(), Some("claude · backend"));
    assert_eq!(scheduled_lit(&state), Some(true));

    // Working or not, it keeps its schedule; the next card replaces the last.
    state.set_snapshot(Box::new({
        let mut working = scheduled_snapshot(TabRemindInterval::M5);
        working.tabs[1].agent_status = AgentStatus::Working;
        working.agents[1].agent_status = AgentStatus::Working;
        working.agents[1].state_change_seq = 5;
        working
    }));
    assert!(state.tick_notifications(t0 + 9 * MINUTE).0.is_empty());
    let (effects, _) = state.tick_notifications(t0 + 10 * MINUTE);
    assert_eq!(sounds(&effects).len(), 1);
    assert_eq!(cards(&state), [reminder_card()]);
}

#[test]
fn interval_reminder_resumes_after_a_gap_without_catching_up() {
    let mut state = seen_state(scheduled_snapshot(TabRemindInterval::M5));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    let (effects, _) = state.tick_notifications(t0 + 60 * MINUTE);
    assert_eq!(sounds(&effects).len(), 1, "one reminder, not twelve");
    assert!(state.tick_notifications(t0 + 64 * MINUTE).0.is_empty());
    assert_eq!(
        sounds(&state.tick_notifications(t0 + 65 * MINUTE).0).len(),
        1
    );
}

#[test]
fn a_reminder_due_while_the_tab_is_focused_is_skipped_and_restarts() {
    let mut state = seen_state(scheduled_snapshot(TabRemindInterval::M5));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    state.set_snapshot(Box::new(with_tab_2_focused(scheduled_snapshot(
        TabRemindInterval::M5,
    ))));
    assert!(state.tick_notifications(t0 + 5 * MINUTE).0.is_empty());
    assert!(cards(&state).is_empty());
    assert_eq!(scheduled_lit(&state), Some(false));
    state.set_snapshot(Box::new(scheduled_snapshot(TabRemindInterval::M5)));
    assert!(state.tick_notifications(t0 + 9 * MINUTE).0.is_empty());
    assert_eq!(
        sounds(&state.tick_notifications(t0 + 10 * MINUTE).0).len(),
        1
    );
}

#[test]
fn focus_clears_the_lit_marker_and_unmarking_removes_the_card() {
    let mut state = seen_state(scheduled_snapshot(TabRemindInterval::M10));
    let t0 = Instant::now();
    state.tick_notifications(t0);
    state.tick_notifications(t0 + 10 * MINUTE);
    assert_eq!(scheduled_lit(&state), Some(true));
    state.set_snapshot(Box::new(with_tab_2_focused(scheduled_snapshot(
        TabRemindInterval::M10,
    ))));
    state.tick_notifications(t0 + 11 * MINUTE);
    assert_eq!(
        scheduled_lit(&state),
        Some(false),
        "cleared, still scheduled"
    );

    state.set_snapshot(Box::new(scheduled_snapshot(TabRemindInterval::M10)));
    state.tick_notifications(t0 + 20 * MINUTE);
    assert_eq!(cards(&state).len(), 1);
    let (_, repaint) = {
        state.set_snapshot(Box::new(waiting_snapshot(AgentStatus::Idle, false)));
        state.tick_notifications(t0 + 21 * MINUTE)
    };
    assert!(repaint);
    assert!(cards(&state).is_empty(), "unmarking takes the card along");
    assert_eq!(scheduled_lit(&state), None);
}

#[test]
fn a_plain_shell_tab_is_reminded_with_its_directory() {
    let mut snapshot = scheduled_snapshot(TabRemindInterval::M5);
    snapshot.agents.truncate(1);
    let mut pane = snapshot.panes[0].clone();
    pane.pane_id = "pane_2".into();
    pane.tab_id = "tab_2".into();
    pane.cwd = Some("/home/me/notes".into());
    pane.focused = false;
    snapshot.panes.push(pane);
    let mut state = seen_state(snapshot);
    let t0 = Instant::now();
    state.tick_notifications(t0);
    let (effects, _) = state.tick_notifications(t0 + 5 * MINUTE);
    assert_eq!(sounds(&effects), [Sound::Reminder(ReminderBase::Done)]);
    let card = state.visible_notifications.back().expect("card");
    assert_eq!(card.event.title, "planner reminder");
    assert_eq!(card.event.body.as_deref(), Some("notes"));
    assert_eq!(card.event.pane_id.as_deref(), Some("pane_2"));
}

fn local(day: u8, hour: u8, minute: u8) -> time::PrimitiveDateTime {
    time::Date::from_calendar_date(2026, time::Month::September, day)
        .unwrap()
        .with_hms(hour, minute, 0)
        .unwrap()
}

#[test]
fn daily_reminder_fires_at_the_time_of_day_and_once_for_a_missed_one() {
    let t0 = Instant::now();
    // First seen after 09:30 (a client attaching late): today's fires once.
    let mut state = seen_state(scheduled_snapshot(TabRemindInterval::Daily));
    state.reminder_local_time = Some(local(27, 10, 0));
    assert_eq!(sounds(&state.tick_notifications(t0).0).len(), 1);
    assert_eq!(cards(&state), [reminder_card()]);
    state.reminder_local_time = Some(local(27, 18, 0));
    assert!(state.tick_notifications(t0 + 8 * 60 * MINUTE).0.is_empty());
    // Next day: nothing before 09:30, one at 09:30.
    state.reminder_local_time = Some(local(28, 9, 29));
    assert!(state.tick_notifications(t0 + 24 * 60 * MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(28, 9, 30));
    assert_eq!(
        sounds(&state.tick_notifications(t0 + 24 * 60 * MINUTE).0).len(),
        1
    );
    assert!(state.tick_notifications(t0 + 25 * 60 * MINUTE).0.is_empty());

    // Focused at 09:30: that day's is skipped.
    state.set_snapshot(Box::new(with_tab_2_focused(scheduled_snapshot(
        TabRemindInterval::Daily,
    ))));
    state.reminder_local_time = Some(local(29, 9, 30));
    assert!(state.tick_notifications(t0 + 48 * 60 * MINUTE).0.is_empty());
    state.set_snapshot(Box::new(scheduled_snapshot(TabRemindInterval::Daily)));
    state.reminder_local_time = Some(local(29, 12, 0));
    assert!(state.tick_notifications(t0 + 50 * 60 * MINUTE).0.is_empty());
}

#[test]
fn a_daily_reminder_set_during_the_day_waits_for_the_next_one() {
    let t0 = Instant::now();
    let mut state = seen_state(waiting_snapshot(AgentStatus::Idle, false));
    state.reminder_local_time = Some(local(27, 15, 0));
    state.tick_notifications(t0);
    state.set_snapshot(Box::new(scheduled_snapshot(TabRemindInterval::Daily)));
    assert!(state.tick_notifications(t0 + MINUTE).0.is_empty());
    assert!(state.tick_notifications(t0 + 2 * MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(28, 9, 30));
    assert_eq!(
        sounds(&state.tick_notifications(t0 + 20 * 60 * MINUTE).0).len(),
        1
    );

    // Before 09:30 on the day it is set, it fires that morning.
    let mut state = seen_state(waiting_snapshot(AgentStatus::Idle, false));
    state.reminder_local_time = Some(local(27, 8, 0));
    state.tick_notifications(t0);
    state.set_snapshot(Box::new(scheduled_snapshot(TabRemindInterval::Daily)));
    assert!(state.tick_notifications(t0 + MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(27, 9, 30));
    assert_eq!(
        sounds(&state.tick_notifications(t0 + 90 * MINUTE).0).len(),
        1
    );
}

#[test]
fn a_tab_can_be_important_and_scheduled_at_once() {
    let mut snapshot = waiting_snapshot(AgentStatus::Done, true);
    snapshot.tabs[1].remind_every = Some(TabRemindInterval::M30);
    let mut state = reminder_state(10, snapshot);
    let t0 = Instant::now();
    state.tick_notifications(t0);
    state.tick_notifications(t0 + 10 * MINUTE);
    assert_eq!(cards(&state)[0].0, "planner finished 10 min ago");
    state.tick_notifications(t0 + 30 * MINUTE);
    assert_eq!(cards(&state), [reminder_card()], "the tab keeps one card");
    assert!(state
        .idle_reminders
        .contains_key(&(ClientEndpointId::Local, "tab_2".to_string())));
    assert_eq!(scheduled_lit(&state), Some(true));
}

#[test]
fn tab_menu_has_important_and_the_selector_before_the_swatches() {
    let mut snapshot = waiting_snapshot(AgentStatus::Done, true);
    snapshot.tabs[1].remind_every = Some(TabRemindInterval::H6);
    let mut state = tabs_state(snapshot);
    let items = open_tab_menu(&mut state, 1);
    let close = items
        .iter()
        .position(|item| item.action == ClientContextMenuAction::Close)
        .unwrap();
    assert_eq!(
        items[close..]
            .iter()
            .map(|item| item.action)
            .collect::<Vec<_>>(),
        [
            ClientContextMenuAction::Close,
            ClientContextMenuAction::Important,
            ClientContextMenuAction::RemindTop,
            ClientContextMenuAction::RemindBottom,
            ClientContextMenuAction::Color
        ]
    );
    assert_eq!(items[close + 1].label, "\u{2713} important");
    assert_eq!(
        menu_remind(&state).1,
        ClientTabMenuRemind {
            current: Some(TabRemindInterval::H6),
            cursor: 4
        }
    );
    let frame = state.compose(106, 20).expect("menu frame");
    let top = row_text(&frame, state.hits.context_menu_rows[close + 2].0);
    let bottom = row_text(&frame, state.hits.context_menu_rows[close + 3].0);
    assert!(top.starts_with("remind  5m  10m  30m"), "{top:?}");
    assert!(bottom.starts_with("        1h [6h]  daily"), "{bottom:?}");
    let options = state.hits.context_menu_remind_options.clone();
    assert_eq!(
        options.iter().map(|(_, index)| *index).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 5]
    );
    // Columns line up across the two rows.
    for column in 0..3 {
        assert_eq!(options[column].0.x, options[column + 3].0.x);
    }

    let mut state = tabs_state(waiting_snapshot(AgentStatus::Done, false));
    let items = open_tab_menu(&mut state, 1);
    assert!(items.iter().any(|item| item.label == "  important"));
}

#[test]
fn the_important_item_toggles_without_focusing_the_tab() {
    for important in [false, true] {
        let mut state = tabs_state(waiting_snapshot(AgentStatus::Done, important));
        let items = open_tab_menu(&mut state, 1);
        let row = items
            .iter()
            .position(|item| item.action == ClientContextMenuAction::Important)
            .unwrap();
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(row, &mut outcome);
        assert_eq!(
            reminders_sent(&outcome),
            [("tab_2".to_string(), Some(!important), None)]
        );
        assert!(no_tab_focus(&outcome));
        assert!(state.overlay.is_none());
    }
}

#[test]
fn selector_clicks_pick_an_interval_and_the_current_one_turns_it_off() {
    for (current, clicked, expected) in [
        (None, 0, TabRemindEvery::M5),
        (None, 5, TabRemindEvery::Daily),
        (Some(TabRemindInterval::H1), 4, TabRemindEvery::H6),
        (Some(TabRemindInterval::M30), 2, TabRemindEvery::Off),
        (Some(TabRemindInterval::Daily), 5, TabRemindEvery::Off),
    ] {
        let mut snapshot = waiting_snapshot(AgentStatus::Done, false);
        snapshot.tabs[1].remind_every = current;
        let mut state = tabs_state(snapshot);
        open_tab_menu(&mut state, 1);
        state.compose(106, 20).expect("menu frame");
        let (rect, _) = state.hits.context_menu_remind_options[clicked];
        let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 1,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert_eq!(
            reminders_sent(&outcome),
            [("tab_2".to_string(), None, Some(expected))],
            "{current:?} click {clicked}"
        );
        assert!(no_tab_focus(&outcome));
        assert!(state.overlay.is_none());
    }
}

#[test]
fn selector_keys_move_across_both_rows_and_pick() {
    use crossterm::event::KeyCode;
    let mut state = tabs_state(waiting_snapshot(AgentStatus::Done, false));
    let items = open_tab_menu(&mut state, 1);
    let top = items
        .iter()
        .position(|item| item.action == ClientContextMenuAction::RemindTop)
        .unwrap();
    // Entering from above lands on the first row's first column.
    state.handle_raw_events((0..top).map(|_| key(KeyCode::Down)).collect());
    assert_eq!(
        menu_remind(&state),
        (
            top,
            ClientTabMenuRemind {
                current: None,
                cursor: 0
            }
        )
    );
    state.handle_raw_events(vec![
        key(KeyCode::Right),
        key(KeyCode::Right),
        key(KeyCode::Right),
    ]);
    assert_eq!(menu_remind(&state).1.cursor, 2, "stops at the row's end");
    // Down keeps the column on the second row; Left moves within it.
    state.handle_raw_events(vec![key(KeyCode::Down)]);
    assert_eq!(menu_remind(&state).0, top + 1);
    assert_eq!(menu_remind(&state).1.cursor, 5);
    state.handle_raw_events(vec![key(KeyCode::Char('h'))]);
    assert_eq!(menu_remind(&state).1.cursor, 4);
    state.handle_raw_events(vec![key(KeyCode::Up)]);
    assert_eq!(
        menu_remind(&state),
        (
            top,
            ClientTabMenuRemind {
                current: None,
                cursor: 1
            }
        )
    );
    state.handle_raw_events(vec![key(KeyCode::Down)]);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert_eq!(
        reminders_sent(&outcome),
        [("tab_2".to_string(), None, Some(TabRemindEvery::H6))]
    );

    // Past the rows: Down to the swatches, Up from them enters the second
    // row on the current interval.
    let mut snapshot = waiting_snapshot(AgentStatus::Done, false);
    snapshot.tabs[1].remind_every = Some(TabRemindInterval::Daily);
    let mut state = tabs_state(snapshot);
    let items = open_tab_menu(&mut state, 1);
    state.handle_raw_events((0..items.len()).map(|_| key(KeyCode::Down)).collect());
    assert_eq!(menu_remind(&state).0, items.len() - 1, "on the swatch row");
    state.handle_raw_events(vec![key(KeyCode::Up)]);
    assert_eq!(menu_remind(&state).0, top + 1);
    assert_eq!(menu_remind(&state).1.cursor, 5);
    state.handle_raw_events(vec![key(KeyCode::Up), key(KeyCode::Up)]);
    assert_eq!(
        menu_remind(&state).0,
        top - 1,
        "past the first row: important"
    );
    let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert_eq!(
        reminders_sent(&outcome),
        [("tab_2".to_string(), Some(true), None)]
    );
}

#[test]
fn toggle_tab_important_binding_flips_the_focused_tab() {
    use crate::input::{KeybindAction, KeybindMatch};
    let mut snapshot = waiting_snapshot(AgentStatus::Done, false);
    for expected in [true, false] {
        snapshot.tabs[0].important = !expected;
        let mut state = tabs_state(snapshot.clone());
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            KeybindMatch::Action(KeybindAction::ToggleTabImportant),
            &mut outcome,
        );
        assert_eq!(
            reminders_sent(&outcome),
            [("tab_1".to_string(), Some(expected), None)]
        );
    }
    for name in ["toggle_tab_important", "toggle_tab_remind"] {
        let bound: Config = toml::from_str(&format!("[keys]\n{name} = \"alt+m\"")).unwrap();
        assert!(
            !bound
                .live_keybinds_with_diagnostics()
                .map(|(keybinds, _)| keybinds.keybinds.toggle_tab_important.bindings.is_empty())
                .unwrap_or(true),
            "{name}"
        );
    }
    assert!(!Config::default().keys.toggle_tab_important.has_values());
}

fn marker_cells(state: &mut ClientShellState) -> Vec<(String, u32)> {
    let frame = state.compose(106, 20).expect("composed frame");
    let (rect, _) = state.hits.sidebar_tabs[1];
    let important = super::super::tab_sidebar::TAB_IMPORTANT_MARKER;
    let remind = super::super::tab_sidebar::TAB_REMIND_MARKER;
    (rect.x..rect.right())
        .map(|x| &frame.cells[(rect.y * frame.width + x) as usize])
        .filter(|cell| cell.symbol == important || cell.symbol == remind)
        .map(|cell| (cell.symbol.clone(), cell.fg))
        .collect()
}

#[test]
fn rows_show_star_and_clock_before_the_glyph_and_light_them_once_fired() {
    use crate::protocol::color_to_u32;
    let mut snapshot = waiting_snapshot(AgentStatus::Done, true);
    snapshot.tabs[1].remind_every = Some(TabRemindInterval::M30);
    let mut config = reminder_config(10, crate::config::ToastDelivery::Herdr);
    config.sidebar_layout = SidebarLayoutConfig::Tabs;
    let mut state = ClientShellState::new(config);
    watch_work_then(&mut state, snapshot.clone());
    state.set_pane_surface(surface());
    let palette = state.config.palette.clone();
    let overlay0 = color_to_u32(palette.overlay0);

    let frame = state.compose(106, 20).expect("composed frame");
    let rows = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| row_text(&frame, *rect))
        .collect::<Vec<_>>();
    assert!(
        rows[1].trim_end().ends_with("\u{2605} \u{25F7} \u{29C6}"),
        "star, clock, glyph: {rows:?}"
    );
    assert!(!rows[0].contains('\u{2605}') && !rows[0].contains('\u{25F7}'));
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(rows[0].as_str()),
        unicode_width::UnicodeWidthStr::width(rows[1].as_str())
    );

    let t0 = Instant::now();
    state.tick_notifications(t0);
    assert_eq!(
        marker_cells(&mut state),
        [
            ("\u{2605}".to_string(), overlay0),
            ("\u{25F7}".to_string(), overlay0)
        ]
    );
    state.tick_notifications(t0 + 10 * MINUTE);
    assert_eq!(
        marker_cells(&mut state),
        [
            ("\u{2605}".to_string(), color_to_u32(palette.teal)),
            ("\u{25F7}".to_string(), overlay0)
        ]
    );
    state.tick_notifications(t0 + 30 * MINUTE);
    assert_eq!(
        marker_cells(&mut state)[1],
        ("\u{25F7}".to_string(), color_to_u32(palette.accent))
    );

    // Focusing the tab clears both.
    state.set_snapshot(Box::new(with_tab_2_focused(snapshot)));
    state.tick_notifications(t0 + 31 * MINUTE);
    assert_eq!(
        marker_cells(&mut state),
        [
            ("\u{2605}".to_string(), overlay0),
            ("\u{25F7}".to_string(), overlay0)
        ]
    );
}

#[test]
fn a_blocked_important_tab_lights_its_star_red() {
    use crate::protocol::color_to_u32;
    let mut config = reminder_config(5, crate::config::ToastDelivery::Herdr);
    config.sidebar_layout = SidebarLayoutConfig::Tabs;
    let mut state = ClientShellState::new(config);
    watch_work_then(&mut state, waiting_snapshot(AgentStatus::Blocked, true));
    state.set_pane_surface(surface());
    let t0 = Instant::now();
    state.tick_notifications(t0);
    state.tick_notifications(t0 + 5 * MINUTE);
    let red = color_to_u32(state.config.palette.red);
    assert_eq!(marker_cells(&mut state), [("\u{2605}".to_string(), red)]);
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
        7,
        "every choice is drawn, then the daily time row"
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
    assert_eq!(state.hits.settings_choices.len(), 8);
    assert!(
        text.contains("daily at 09:30"),
        "the daily row still fits: {text}"
    );
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

fn open_sound_section(state: &mut ClientShellState) {
    state.open_settings_overlay();
    let index = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::Sound)
        .expect("sound section");
    for _ in 0..index {
        state.handle_input_bytes(b"\t");
    }
}

fn picker(state: &ClientShellState) -> Option<super::super::settings_sounds::ClientSoundPicker> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Settings(settings)) => settings.sound_picker.clone(),
        _ => None,
    }
}

fn unique_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sound_section_lists_on_off_and_one_row_per_sound() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.sound_files[2] = Some("/System/Library/Sounds/Glass.aiff".into());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    open_sound_section(&mut state);
    assert_eq!(
        settings_selection(&state),
        (ClientSettingsSection::Sound, 0)
    );
    let frame = state.compose(106, 30).expect("settings frame");
    let text = frame_rows(&frame).join("\n");
    for label in [
        "on ✓",
        "off",
        "finished: default",
        "needs input: default",
        "reminder: Glass",
    ] {
        assert!(text.contains(label), "{label}: {text}");
    }
    assert_eq!(state.hits.settings_choices.len(), 5);
}

#[test]
fn sound_picker_lists_default_and_the_system_sounds_previews_and_writes_the_key() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = unique_dir("sound-picker");
    let sounds = dir.join("Sounds");
    std::fs::create_dir_all(&sounds).unwrap();
    for name in ["Tink.aiff", "Basso.aiff", "Glass.aiff"] {
        std::fs::write(sounds.join(name), b"").unwrap();
    }
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui.sound]\nenabled = true\n").unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

    let mut config = ClientShellConfig::from_config(&Config::default());
    config.system_sounds_dir = sounds.clone();
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    open_sound_section(&mut state);
    // Down to "reminder: default" (row 4), Enter opens its picker.
    for _ in 0..4 {
        state.handle_input_bytes(b"j");
    }
    state.handle_input_bytes(b"\r");
    let opened = picker(&state).expect("picker open");
    assert_eq!(
        opened
            .choices
            .iter()
            .map(|choice| choice.label.as_str())
            .collect::<Vec<_>>(),
        ["default", "Basso", "Glass", "Tink"]
    );
    assert_eq!(
        settings_selection(&state).1,
        0,
        "starts on the configured one"
    );
    let frame = state.compose(106, 30).expect("picker frame");
    assert!(frame_rows(&frame).join("\n").contains("reminder sound"));

    // Moving previews the sound under the cursor once, without saving.
    let outcome = state.handle_input_bytes(b"jj");
    let previews = outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::PreviewSound { path, fallback } => Some((path.clone(), *fallback)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        previews,
        [
            (Some(sounds.join("Basso.aiff")), Sound::Done),
            (Some(sounds.join("Glass.aiff")), Sound::Done),
        ]
    );
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains("reminder_path"));

    // Enter keeps it and returns to the rows.
    state.handle_input_bytes(b"\r");
    let written: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        written.ui.sound.reminder_path,
        Some(sounds.join("Glass.aiff"))
    );
    assert!(picker(&state).is_none());
    assert_eq!(
        settings_selection(&state),
        (ClientSettingsSection::Sound, 4)
    );
    assert_eq!(state.config.sound_files[2], Some(sounds.join("Glass.aiff")));

    // Reopen: the cursor is on Glass; Esc goes back without closing settings.
    state.handle_input_bytes(b"\r");
    assert_eq!(settings_selection(&state).1, 2);
    state.handle_raw_events(vec![key(crossterm::event::KeyCode::Esc)]);
    assert!(picker(&state).is_none());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(_))
    ));

    // `default` removes the key.
    state.handle_input_bytes(b"\r");
    state.handle_input_bytes(b"k");
    state.handle_input_bytes(b"k");
    state.handle_input_bytes(b"\r");
    let content = std::fs::read_to_string(&path).unwrap();
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!content.contains("reminder_path"), "{content}");
    assert_eq!(state.config.sound_files[2], None);
}

#[test]
fn sound_picker_without_the_system_sounds_offers_only_default() {
    let choices = super::super::settings_sounds::sound_choices(std::path::Path::new(
        "/nonexistent/herdr/sounds",
    ));
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].label, "default");
    assert_eq!(choices[0].path, None);
}

#[test]
fn reminder_cards_carry_which_reminder_raised_them() {
    use super::super::idle_reminders::ClientReminderKind;
    let reminder_of =
        |state: &ClientShellState| state.visible_notifications.back().expect("card").reminder;
    let t0 = Instant::now();
    let mut state = reminder_state(10, waiting_snapshot(AgentStatus::Blocked, true));
    state.tick_notifications(t0);
    state.tick_notifications(t0 + 10 * MINUTE);
    assert_eq!(reminder_of(&state), Some(ClientReminderKind::Important));

    let mut state = seen_state(scheduled_snapshot(TabRemindInterval::M5));
    state.tick_notifications(t0);
    state.tick_notifications(t0 + 5 * MINUTE);
    assert_eq!(reminder_of(&state), Some(ClientReminderKind::Scheduled));

    // A server notification is not a reminder.
    let mut state = seen_state(waiting_snapshot(AgentStatus::Blocked, false));
    state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "claude needs attention".into(),
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
    assert_eq!(reminder_of(&state), None);
}

#[test]
fn changing_the_daily_time_moves_the_next_firing_without_firing_now() {
    let t0 = Instant::now();
    let mut state = seen_state(scheduled_snapshot(TabRemindInterval::Daily));
    // Seen at 10:00: today's 09:30 fires once.
    state.reminder_local_time = Some(local(27, 10, 0));
    assert_eq!(sounds(&state.tick_notifications(t0).0).len(), 1);

    // Moved to 15:00 at 12:00: still ahead today, so it fires at 15:00.
    state.config.daily_reminder_minutes = 15 * 60;
    state.reminder_local_time = Some(local(27, 12, 0));
    assert!(state.tick_notifications(t0 + MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(27, 14, 59));
    assert!(state.tick_notifications(t0 + 2 * MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(27, 15, 0));
    assert_eq!(
        sounds(&state.tick_notifications(t0 + 3 * MINUTE).0).len(),
        1
    );

    // Moved to 08:00 at 16:00: already past, so nothing until tomorrow 08:00.
    state.config.daily_reminder_minutes = 8 * 60;
    state.reminder_local_time = Some(local(27, 16, 0));
    assert!(state.tick_notifications(t0 + 4 * MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(27, 23, 0));
    assert!(state.tick_notifications(t0 + 5 * MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(28, 7, 59));
    assert!(state.tick_notifications(t0 + 6 * MINUTE).0.is_empty());
    state.reminder_local_time = Some(local(28, 8, 0));
    assert_eq!(
        sounds(&state.tick_notifications(t0 + 7 * MINUTE).0).len(),
        1
    );

    // Moved earlier than now before today's fired: no immediate firing.
    state.config.daily_reminder_minutes = 20 * 60;
    state.reminder_local_time = Some(local(28, 12, 0));
    assert!(state.tick_notifications(t0 + 8 * MINUTE).0.is_empty());
    state.config.daily_reminder_minutes = 11 * 60;
    state.reminder_local_time = Some(local(29, 9, 0));
    state.tick_notifications(t0 + 9 * MINUTE);
    state.config.daily_reminder_minutes = 8 * 60;
    assert!(state.tick_notifications(t0 + 10 * MINUTE).0.is_empty());
}

fn daily_picker(state: &ClientShellState) -> Option<Vec<(String, u32)>> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Settings(settings)) => settings.daily_time_picker.clone(),
        _ => None,
    }
}

fn reminders_state(daily_minutes: u32) -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.daily_reminder_minutes = daily_minutes;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    open_reminders_section(&mut state);
    state
}

#[test]
fn reminders_section_shows_the_daily_time_row_and_its_picker() {
    let mut state = reminders_state(9 * 60 + 30);
    let frame = state.compose(106, 30).expect("settings frame");
    assert!(frame_rows(&frame).join("\n").contains("daily at 09:30"));
    // Six intervals, then the daily row.
    assert_eq!(state.hits.settings_choices.len(), 7);
    for _ in 0..4 {
        state.handle_input_bytes(b"j");
    }
    assert_eq!(
        settings_selection(&state),
        (ClientSettingsSection::Reminders, 6)
    );
    state.handle_input_bytes(b"\r");
    let picker = daily_picker(&state).expect("picker open");
    assert_eq!(picker.len(), 48);
    assert_eq!(picker[0], ("00:00".to_string(), 0));
    assert_eq!(picker[19], ("09:30".to_string(), 570));
    assert_eq!(picker[47], ("23:30".to_string(), 23 * 60 + 30));
    assert!(picker
        .iter()
        .all(|(label, _)| label.len() == 5 && label.as_bytes()[2] == b':'));
    assert_eq!(settings_selection(&state).1, 19, "the configured time");
    let frame = state.compose(106, 30).expect("picker frame");
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("09:30 ✓"), "{text}");
    assert!(text.contains("daily reminder time"), "{text}");

    // Esc goes back to the rows, on the daily row, settings still open.
    state.handle_raw_events(vec![key(crossterm::event::KeyCode::Esc)]);
    assert!(daily_picker(&state).is_none());
    assert_eq!(
        settings_selection(&state),
        (ClientSettingsSection::Reminders, 6)
    );
}

#[test]
fn a_daily_time_off_the_grid_shows_first_as_custom() {
    let mut state = reminders_state(9 * 60 + 45);
    let frame = state.compose(106, 30).expect("settings frame");
    assert!(frame_rows(&frame).join("\n").contains("daily at 09:45"));
    let row = state.hits.settings_choices.len() - 1;
    state.select_settings_choice(row);
    state.handle_input_bytes(b"\r");
    let picker = daily_picker(&state).expect("picker open");
    assert_eq!(picker.len(), 49);
    assert_eq!(picker[0], ("custom: 09:45".to_string(), 585));
    assert_eq!(settings_selection(&state).1, 0);
    let frame = state.compose(106, 30).expect("picker frame");
    assert!(frame_rows(&frame).join("\n").contains("custom: 09:45 ✓"));
}

#[test]
fn picking_a_daily_time_writes_the_key_and_reloads_it() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = unique_dir("daily-time");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\nidle_reminder_minutes = 10\n").unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

    let mut state = reminders_state(9 * 60 + 30);
    state.compose(106, 30).expect("settings frame");
    let row = state.hits.settings_choices.len() - 1;
    // A click on the daily row opens the picker at once.
    let (rect, _) = state.hits.settings_choices[row];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(daily_picker(&state).is_some());
    // Esc does not write.
    state.handle_raw_events(vec![key(crossterm::event::KeyCode::Esc)]);
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains("daily_reminder_time"));
    // Reopen on 09:30, four steps down to 11:30, Enter writes it.
    state.handle_input_bytes(b"\r");
    for _ in 0..4 {
        state.handle_input_bytes(b"j");
    }
    state.handle_input_bytes(b"\r");
    let written = std::fs::read_to_string(&path).unwrap();
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        written.contains("daily_reminder_time = \"11:30\""),
        "{written}"
    );
    let parsed: Config = toml::from_str(&written).unwrap();
    assert_eq!(parsed.ui.idle_reminder_minutes, 10);
    assert_eq!(
        state.config.daily_reminder_minutes,
        11 * 60 + 30,
        "live reload"
    );
    assert!(daily_picker(&state).is_none());
    assert_eq!(
        settings_selection(&state),
        (ClientSettingsSection::Reminders, 6)
    );
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
        assert_eq!(sounds(&effects), [Sound::Reminder(ReminderBase::Done)]);
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
