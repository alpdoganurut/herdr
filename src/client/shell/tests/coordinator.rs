//! The coordinator's client pieces (fork): the pinned row's state, the pull
//! schedule, the menu and settings actions, and the `tabs` sidebar's pinned
//! row and pinned-tab exclusion (agents model v2: no managed marks).

use super::super::coordinator::{
    coordinator_row_state, dashboard_url_notice, watched_count, ClientCoordinatorState,
    CoordinatorEffect, CoordinatorMenuAction, CoordinatorRequest, CoordinatorRequestKind,
    CoordinatorRow, CoordinatorRowState, COORDINATOR_REFRESH_INTERVAL,
};
use super::super::settings_coordinator::{
    section_rows, ClientCoordinatorSettings, CoordinatorPicker, CoordinatorSettingsRow,
};
use super::super::tab_sidebar::{
    fixed_rows_that_fit, render_tab_sidebar_with, FixedRow, TabSidebarCoordinator,
};
use super::*;
use crate::api::schema::coordinator::{
    CoordinatorGetInfo, CoordinatorManagedInfo, CoordinatorStateInfo, CoordinatorTurnInfo,
};
use crate::api::schema::{Method, ResponseResult};
use crate::config::{Config, SidebarLayoutConfig};

fn info() -> CoordinatorGetInfo {
    CoordinatorGetInfo {
        enabled: true,
        state: CoordinatorStateInfo::Running,
        tab_id: Some("tab_c".into()),
        pane_id: Some("pane_c".into()),
        dashboard_url: Some("http://127.0.0.1:7718/".into()),
        managed: vec![
            managed("calendar-fix", Some("tab_2")),
            managed("reviewer", Some("tab_3")),
        ],
        coordinator_dir: "/tmp/coordinator".into(),
        ..CoordinatorGetInfo::default()
    }
}

fn managed(name: &str, tab_id: Option<&str>) -> CoordinatorManagedInfo {
    CoordinatorManagedInfo {
        name: name.into(),
        tab_id: tab_id.map(str::to_owned),
        ..CoordinatorManagedInfo::default()
    }
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

/// One space: the focused `tab_1`, two managed agents and the coordinator.
fn coordinator_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.tabs = vec![
        tab("tab_1", "ws_1", "main", true),
        tab("tab_2", "ws_1", "calendar-fix", false),
        tab("tab_3", "ws_1", "reviewer", false),
        tab("tab_c", "ws_1", "coordinator", false),
    ];
    snapshot
}

fn state_with(info: CoordinatorGetInfo, snapshot: &ClientShellSnapshot) -> ClientCoordinatorState {
    let mut state = ClientCoordinatorState::default();
    state.on_reply(CoordinatorRequestKind::Get, Some(info), Some(snapshot));
    state
}

fn row_of(info: &CoordinatorGetInfo) -> (CoordinatorRowState, String) {
    coordinator_row_state(info, Some(AgentStatus::Idle), false)
}

#[test]
fn row_state_follows_the_priority_table() {
    let base = info();
    assert_eq!(
        row_of(&base),
        (CoordinatorRowState::Idle, "2 watched".into())
    );

    let mut capped = base.clone();
    capped.wake.capped = true;
    assert_eq!(row_of(&capped).0, CoordinatorRowState::Capped);

    let mut ideas = capped.clone();
    ideas.unread_suggestions = 3;
    assert_eq!(
        row_of(&ideas),
        (CoordinatorRowState::Ideas, "3 ideas".into())
    );
    // The server clears unread on focus: the focused tab does not show them.
    assert_eq!(
        coordinator_row_state(&ideas, Some(AgentStatus::Idle), true).0,
        CoordinatorRowState::Capped
    );

    let mut waking = ideas.clone();
    waking.turn = Some(CoordinatorTurnInfo {
        source: "wake".into(),
        id: "7".into(),
        started_at: 1,
    });
    assert_eq!(
        row_of(&waking),
        (CoordinatorRowState::Waking, "waking".into())
    );
    assert_eq!(
        coordinator_row_state(&waking, Some(AgentStatus::Working), false),
        (CoordinatorRowState::Working, "working".into())
    );

    let mut unavailable = waking.clone();
    unavailable.state = CoordinatorStateInfo::Unavailable;
    assert_eq!(
        row_of(&unavailable),
        (CoordinatorRowState::Unavailable, "n/a".into())
    );

    let mut blocked = waking.clone();
    blocked.state = CoordinatorStateInfo::Blocked;
    blocked.blocked_reason = Some("locked_elsewhere".into());
    assert_eq!(
        row_of(&blocked),
        (CoordinatorRowState::Blocked, "locked".into())
    );
    blocked.blocked_reason = Some("registry_corrupt".into());
    assert_eq!(row_of(&blocked).1, "registry");

    let mut down = blocked.clone();
    down.state = CoordinatorStateInfo::Down;
    assert_eq!(row_of(&down), (CoordinatorRowState::Down, "down".into()));

    let mut off = base.clone();
    off.enabled = false;
    off.state = CoordinatorStateInfo::Off;
    assert_eq!(row_of(&off), (CoordinatorRowState::Off, "off".into()));
}

#[test]
fn a_coordinator_waiting_on_the_user_needs_you_above_working_and_ideas() {
    let mut running = info();
    running.unread_suggestions = 2;
    running.turn = Some(CoordinatorTurnInfo {
        source: "wake".into(),
        id: "3".into(),
        started_at: 1,
    });
    let needs_you = (CoordinatorRowState::NeedsYou, "needs you".to_string());
    // Running: a prompt (blocked) or an unknown state needs the user.
    assert_eq!(
        coordinator_row_state(&running, Some(AgentStatus::Blocked), false),
        needs_you
    );
    assert_eq!(
        coordinator_row_state(&running, Some(AgentStatus::Unknown), false),
        needs_you
    );
    assert_eq!(
        coordinator_row_state(&running, Some(AgentStatus::Working), false).0,
        CoordinatorRowState::Working
    );
    // Without the tab in the snapshot, the reply's status decides.
    running.coordinator_status = Some("blocked".into());
    assert_eq!(coordinator_row_state(&running, None, false), needs_you);

    // Starting: blocked on a prompt (Claude's folder trust) needs the user;
    // unknown is the moment before the agent paints.
    let mut starting = info();
    starting.state = CoordinatorStateInfo::Starting;
    starting.unread_suggestions = 1;
    assert_eq!(
        coordinator_row_state(&starting, Some(AgentStatus::Blocked), false),
        needs_you
    );
    assert_eq!(
        coordinator_row_state(&starting, Some(AgentStatus::Unknown), false).0,
        CoordinatorRowState::Ideas
    );

    // Down and the server-side blocks still come first.
    let mut down = running.clone();
    down.state = CoordinatorStateInfo::Down;
    assert_eq!(
        coordinator_row_state(&down, Some(AgentStatus::Blocked), false).0,
        CoordinatorRowState::Down
    );
    assert_eq!(CoordinatorRowState::NeedsYou.glyph(), "!");
}

#[test]
fn clicking_the_needs_you_row_focuses_the_coordinator_tab() {
    let mut snapshot = coordinator_snapshot();
    let state = state_with(info(), &snapshot);
    for tab in &mut snapshot.tabs {
        if tab.tab_id == "tab_c" {
            tab.agent_status = AgentStatus::Blocked;
        }
    }
    let row = state.row(&snapshot).expect("row");
    assert_eq!(row.state, CoordinatorRowState::NeedsYou);
    assert_eq!(row.tab_mark(), "! needs you");
    assert_eq!(
        state.activate_row(&snapshot),
        CoordinatorEffect::FocusTab("tab_c".into())
    );
}

#[test]
fn a_queued_wake_reply_says_why_and_is_not_kept() {
    let snapshot = coordinator_snapshot();
    let mut state = state_with(info(), &snapshot);
    let mut queued = info();
    queued.wake_queued = Some("coordinator is working".into());
    assert_eq!(
        state.on_reply(CoordinatorRequestKind::Wake, Some(queued), Some(&snapshot)),
        Some("wake queued: coordinator is working".into())
    );
    assert_eq!(state.info.as_ref().expect("info").wake_queued, None);
    assert_eq!(
        state.on_reply(CoordinatorRequestKind::Wake, Some(info()), Some(&snapshot)),
        None,
        "delivered now: no notice"
    );
}

#[test]
fn an_unknown_state_reads_as_the_idle_row() {
    let mut newer = info();
    newer.state = CoordinatorStateInfo::Unknown;
    newer.managed.truncate(1);
    assert_eq!(
        row_of(&newer),
        (CoordinatorRowState::Idle, "1 watched".into())
    );
}

#[test]
fn the_row_needs_a_reply_and_an_enabled_coordinator_or_its_tab() {
    let snapshot = coordinator_snapshot();
    assert_eq!(ClientCoordinatorState::default().row(&snapshot), None);

    let state = state_with(info(), &snapshot);
    let row = state.row(&snapshot).expect("row");
    assert_eq!(row.tab_id.as_deref(), Some("tab_c"));
    assert_eq!(row.tab_mark(), "○ 2 watched");

    // Enabled without a tab: the row stays and a click opens one.
    let mut no_tab = info();
    no_tab.tab_id = None;
    let state = state_with(no_tab.clone(), &snapshot);
    assert_eq!(state.row(&snapshot).expect("row").tab_id, None);
    assert_eq!(
        state.activate_row(&snapshot),
        CoordinatorEffect::Request(CoordinatorRequest::Open)
    );

    // Disabled: only while its tab is still listed.
    no_tab.enabled = false;
    no_tab.state = CoordinatorStateInfo::Off;
    assert_eq!(state_with(no_tab, &snapshot).row(&snapshot), None);
    let mut paused = info();
    paused.enabled = false;
    paused.state = CoordinatorStateInfo::Off;
    let state = state_with(paused, &snapshot);
    assert_eq!(state.row(&snapshot).expect("row").status, "off");
    assert_eq!(
        state.activate_row(&snapshot),
        CoordinatorEffect::FocusTab("tab_c".into())
    );
}

#[test]
fn nothing_is_pulled_from_a_server_without_coordinator_get() {
    let snapshot = coordinator_snapshot();
    let mut state = ClientCoordinatorState::default();
    let now = std::time::Instant::now();
    assert_eq!(state.tick(&snapshot, false, true, now), (false, None));
    assert!(!state.loading);
    // Remembered: not asked again on every tick.
    assert_eq!(state.tick(&snapshot, false, true, now), (false, None));
    assert_eq!(state.row(&snapshot), None);
}

#[test]
fn a_coordinator_status_change_pulls_and_the_reply_seats_the_signature() {
    let mut snapshot = coordinator_snapshot();
    let mut state = ClientCoordinatorState::default();
    let now = std::time::Instant::now();
    assert_eq!(
        state.tick(&snapshot, true, true, now),
        (false, Some(CoordinatorRequest::Get))
    );
    // In flight: no second pull.
    assert_eq!(state.tick(&snapshot, true, true, now).1, None);
    state.on_reply(CoordinatorRequestKind::Get, Some(info()), Some(&snapshot));
    // Learning the tab is not itself a change.
    assert_eq!(state.tick(&snapshot, true, true, now).1, None);

    // The wake lands: Idle -> Working on the coordinator tab.
    snapshot.tabs[3].agent_status = AgentStatus::Working;
    assert_eq!(
        state.tick(&snapshot, true, true, now).1,
        Some(CoordinatorRequest::Get)
    );
    state.on_reply(CoordinatorRequestKind::Get, Some(info()), Some(&snapshot));

    // Another tab's status is not a reason.
    snapshot.tabs[1].agent_status = AgentStatus::Working;
    assert_eq!(state.tick(&snapshot, true, true, now).1, None);

    // Focusing the coordinator tab is (the server clears unread then).
    snapshot.tabs[3].focused = true;
    assert_eq!(
        state.tick(&snapshot, true, true, now).1,
        Some(CoordinatorRequest::Get)
    );
    state.get_not_sent();
    assert!(!state.loading);

    // The periodic pull.
    let later = now + COORDINATOR_REFRESH_INTERVAL;
    assert_eq!(
        state.tick(&snapshot, true, true, later).1,
        Some(CoordinatorRequest::Get)
    );
}

#[test]
fn a_new_connection_drops_the_old_servers_state() {
    let snapshot = coordinator_snapshot();
    let mut state = state_with(info(), &snapshot);
    let mut other = snapshot.clone();
    other.boot_id = "boot-2".into();
    let (repaint, request) = state.tick(&other, true, true, std::time::Instant::now());
    assert!(repaint);
    assert_eq!(request, Some(CoordinatorRequest::Get));
    assert!(state.info.is_none());
}

#[test]
fn the_watched_count_leaves_out_the_coordinators_own_entry() {
    let snapshot = coordinator_snapshot();
    let mut with_self = info();
    with_self
        .managed
        .push(managed("coordinator", Some("tab_c")));
    with_self.managed.push(managed("unplaced", None));
    assert_eq!(watched_count(&with_self), 3, "the unplaced one counts");
    let mut by_pane = info();
    by_pane.managed.push(CoordinatorManagedInfo {
        name: "coordinator".into(),
        pane_id: Some("pane_c".into()),
        ..CoordinatorManagedInfo::default()
    });
    assert_eq!(watched_count(&by_pane), 2);
    let state = state_with(with_self, &snapshot);
    assert_eq!(state.row(&snapshot).expect("row").status, "3 watched");
    assert!(state.is_coordinator_tab("tab_c"));
    assert!(!state.is_coordinator_tab("tab_2"));
}

#[test]
fn open_dashboard_opens_on_local_and_shows_the_url_on_remote() {
    let snapshot = coordinator_snapshot();
    let mut state = state_with(info(), &snapshot);
    assert_eq!(
        state.activate_menu(CoordinatorMenuAction::OpenDashboard, &snapshot, false),
        CoordinatorEffect::Request(CoordinatorRequest::OpenDashboard { open: true })
    );
    let remote = state.activate_menu(CoordinatorMenuAction::OpenDashboard, &snapshot, true);
    assert_eq!(
        remote,
        CoordinatorEffect::Request(CoordinatorRequest::OpenDashboard { open: false })
    );
    let CoordinatorEffect::Request(request) = remote else {
        unreachable!()
    };
    assert_eq!(request.method_name(), "coordinator.open_dashboard");

    // The reply clears unread and, with open: false, hands back the URL.
    let mut read = info();
    read.unread_suggestions = 0;
    assert_eq!(
        state.on_reply(request.kind(), Some(read.clone()), Some(&snapshot)),
        Some(dashboard_url_notice("http://127.0.0.1:7718/"))
    );
    assert_eq!(
        state.on_reply(
            CoordinatorRequestKind::OpenDashboard { open: true },
            Some(read),
            Some(&snapshot)
        ),
        None
    );
    // A refusal pulls again.
    assert_eq!(
        state.on_reply(
            CoordinatorRequestKind::OpenDashboard { open: true },
            None,
            Some(&snapshot)
        ),
        None
    );
    assert!(state.refresh_due);
}

#[test]
fn open_dashboard_is_disabled_with_the_reason_without_a_url() {
    let snapshot = coordinator_snapshot();
    let mut busy = info();
    busy.dashboard_url = None;
    busy.dashboard_error = Some("port 7718 in use".into());
    let state = state_with(busy.clone(), &snapshot);
    let item = &state.menu_items()[0];
    assert_eq!(item.action, CoordinatorMenuAction::OpenDashboard);
    assert_eq!(
        item.disabled_reason.as_deref(),
        Some("dashboard unavailable: port 7718 in use")
    );
    assert_eq!(
        state.activate_menu(CoordinatorMenuAction::OpenDashboard, &snapshot, false),
        CoordinatorEffect::Refused("dashboard unavailable: port 7718 in use".into())
    );

    busy.enabled = false;
    busy.dashboard_error = None;
    let state = state_with(busy, &snapshot);
    assert_eq!(
        state.menu_items()[0].disabled_reason.as_deref(),
        Some("coordinator off")
    );
}

#[test]
fn context_menu_actions_map_to_requests() {
    let snapshot = coordinator_snapshot();
    let state = state_with(info(), &snapshot);
    let labels: Vec<&str> = state.menu_items().iter().map(|item| item.label).collect();
    assert_eq!(
        labels,
        [
            "Open dashboard",
            "Focus coordinator",
            "Wake now",
            "Restart coordinator",
            "Pause coordinator",
            "Settings"
        ]
    );
    let act = |action| state.activate_menu(action, &snapshot, false);
    assert_eq!(
        act(CoordinatorMenuAction::Focus),
        CoordinatorEffect::FocusTab("tab_c".into())
    );
    assert_eq!(
        act(CoordinatorMenuAction::WakeNow),
        CoordinatorEffect::Request(CoordinatorRequest::Wake)
    );
    assert_eq!(
        act(CoordinatorMenuAction::Restart),
        CoordinatorEffect::Request(CoordinatorRequest::Start { resume: true })
    );
    assert_eq!(
        act(CoordinatorMenuAction::ToggleEnabled),
        CoordinatorEffect::Request(CoordinatorRequest::SetEnabled(false))
    );
    assert_eq!(
        act(CoordinatorMenuAction::Settings),
        CoordinatorEffect::OpenSettings
    );

    let mut paused = info();
    paused.enabled = false;
    let state = state_with(paused, &snapshot);
    assert_eq!(state.menu_items()[4].label, "Resume coordinator");
    let act = |action| state.activate_menu(action, &snapshot, false);
    assert_eq!(
        act(CoordinatorMenuAction::ToggleEnabled),
        CoordinatorEffect::Request(CoordinatorRequest::SetEnabled(true))
    );
    assert_eq!(
        act(CoordinatorMenuAction::WakeNow),
        CoordinatorEffect::Refused("coordinator off".into())
    );

    // Without a tab, Focus creates one.
    let mut no_tab = info();
    no_tab.tab_id = None;
    let state = state_with(no_tab, &snapshot);
    assert_eq!(
        state.activate_menu(CoordinatorMenuAction::Focus, &snapshot, false),
        CoordinatorEffect::Request(CoordinatorRequest::Open)
    );
    assert!(CoordinatorRequest::Open.changes_focus());
}

#[test]
fn every_request_names_its_method() {
    let names: Vec<&str> = [
        CoordinatorRequest::Get,
        CoordinatorRequest::Open,
        CoordinatorRequest::OpenDashboard { open: true },
        CoordinatorRequest::Wake,
        CoordinatorRequest::Start { resume: false },
        CoordinatorRequest::SetEnabled(true),
        CoordinatorRequest::SetWakeCaps {
            cap_hour: 1,
            cap_day: 2,
        },
        CoordinatorRequest::SetModel(None),
        CoordinatorRequest::SetNotify(true),
    ]
    .iter()
    .map(CoordinatorRequest::method_name)
    .collect();
    assert_eq!(
        names,
        [
            "coordinator.get",
            "coordinator.open",
            "coordinator.open_dashboard",
            "coordinator.wake",
            "coordinator.start",
            "coordinator.set_enabled",
            "coordinator.set_wake_caps",
            "coordinator.set_model",
            "coordinator.set_notify",
        ]
    );
}

fn settings_with(info: CoordinatorGetInfo) -> ClientCoordinatorSettings {
    let snapshot = coordinator_snapshot();
    let shell = state_with(info, &snapshot);
    let mut settings = ClientCoordinatorSettings::default();
    settings.enter(&shell);
    settings.sync(&shell, 0, false);
    settings
}

fn row_index(row: CoordinatorSettingsRow, spaces: bool) -> usize {
    section_rows(spaces)
        .iter()
        .position(|candidate| *candidate == row)
        .expect("row in section")
}

#[test]
fn the_layout_hint_row_shows_only_under_spaces() {
    assert!(!section_rows(false).contains(&CoordinatorSettingsRow::LayoutHint));
    assert!(section_rows(true).contains(&CoordinatorSettingsRow::LayoutHint));
    let mut settings = settings_with(info());
    assert_eq!(settings.row_count(false) + 1, settings.row_count(true));
    let hint = row_index(CoordinatorSettingsRow::LayoutHint, true);
    assert_eq!(
        settings.apply(hint, true, false).effect,
        CoordinatorEffect::SwitchLayoutToTabs
    );
}

#[test]
fn settings_rows_send_the_right_method() {
    let mut base = info();
    base.wake.cap_hour = 12;
    base.wake.cap_day = 80;
    let mut settings = settings_with(base);
    let apply = |settings: &mut ClientCoordinatorSettings, row| {
        settings.apply(row_index(row, false), false, false).effect
    };
    assert_eq!(
        apply(&mut settings, CoordinatorSettingsRow::Enabled),
        CoordinatorEffect::Request(CoordinatorRequest::SetEnabled(false))
    );
    assert_eq!(
        apply(&mut settings, CoordinatorSettingsRow::Notify),
        CoordinatorEffect::Request(CoordinatorRequest::SetNotify(false))
    );
    // The row follows the read model's `notify`, as the reply reports it.
    if let Some(info) = settings.info.as_mut() {
        info.notify = false;
    }
    assert_eq!(
        apply(&mut settings, CoordinatorSettingsRow::Notify),
        CoordinatorEffect::Request(CoordinatorRequest::SetNotify(true))
    );
    assert_eq!(
        apply(&mut settings, CoordinatorSettingsRow::OpenDashboard),
        CoordinatorEffect::Request(CoordinatorRequest::OpenDashboard { open: true })
    );
    assert_eq!(
        settings
            .apply(
                row_index(CoordinatorSettingsRow::OpenDashboard, false),
                false,
                true
            )
            .effect,
        CoordinatorEffect::Request(CoordinatorRequest::OpenDashboard { open: false })
    );
    assert_eq!(
        apply(&mut settings, CoordinatorSettingsRow::WakeNow),
        CoordinatorEffect::Request(CoordinatorRequest::Wake)
    );
    assert_eq!(
        apply(&mut settings, CoordinatorSettingsRow::Restart),
        CoordinatorEffect::Request(CoordinatorRequest::Start { resume: true })
    );

    // Wakes per hour: a picker on the configured cap, then both caps go.
    let outcome = settings.apply(
        row_index(CoordinatorSettingsRow::CapHour, false),
        false,
        false,
    );
    assert_eq!(outcome.effect, CoordinatorEffect::Nothing);
    assert_eq!(outcome.select, Some(2), "12 is the third choice");
    assert!(matches!(
        settings.picker,
        Some(CoordinatorPicker::CapHour(_))
    ));
    assert_eq!(settings.row_count(false), 5);
    let outcome = settings.apply(4, false, false);
    assert_eq!(
        outcome.effect,
        CoordinatorEffect::Request(CoordinatorRequest::SetWakeCaps {
            cap_hour: 30,
            cap_day: 80
        })
    );
    assert_eq!(
        outcome.select,
        Some(row_index(CoordinatorSettingsRow::CapHour, false))
    );
    assert!(settings.picker.is_none());

    // Wakes per day keeps the hourly cap.
    settings.apply(
        row_index(CoordinatorSettingsRow::CapDay, false),
        false,
        false,
    );
    assert_eq!(
        settings.apply(0, false, false).effect,
        CoordinatorEffect::Request(CoordinatorRequest::SetWakeCaps {
            cap_hour: 12,
            cap_day: 20
        })
    );

    // Model: default first; esc goes back to the model row.
    settings.apply(
        row_index(CoordinatorSettingsRow::Model, false),
        false,
        false,
    );
    assert_eq!(
        settings.close_picker(false),
        Some(row_index(CoordinatorSettingsRow::Model, false))
    );
    assert_eq!(settings.close_picker(false), None);
    settings.apply(
        row_index(CoordinatorSettingsRow::Model, false),
        false,
        false,
    );
    assert_eq!(
        settings.apply(1, false, false).effect,
        CoordinatorEffect::Request(CoordinatorRequest::SetModel(Some("opus".into())))
    );
    settings.apply(
        row_index(CoordinatorSettingsRow::Model, false),
        false,
        false,
    );
    assert_eq!(
        settings.apply(0, false, false).effect,
        CoordinatorEffect::Request(CoordinatorRequest::SetModel(None))
    );
}

#[test]
fn a_picked_wake_cap_moves_the_other_one_so_the_pair_stays_valid() {
    let mut tight = info();
    tight.wake.cap_hour = 12;
    tight.wake.cap_day = 20;
    let mut settings = settings_with(tight);
    // 30 an hour over a day cap of 20: the day cap rises with it.
    settings.apply(
        row_index(CoordinatorSettingsRow::CapHour, false),
        false,
        false,
    );
    assert_eq!(
        settings.apply(4, false, false).effect,
        CoordinatorEffect::Request(CoordinatorRequest::SetWakeCaps {
            cap_hour: 30,
            cap_day: 30
        })
    );
    let mut busy = info();
    busy.wake.cap_hour = 30;
    busy.wake.cap_day = 80;
    let mut settings = settings_with(busy);
    // A day cap of 20 under 30 an hour: the hourly cap comes down to it.
    settings.apply(
        row_index(CoordinatorSettingsRow::CapDay, false),
        false,
        false,
    );
    assert_eq!(
        settings.apply(0, false, false).effect,
        CoordinatorEffect::Request(CoordinatorRequest::SetWakeCaps {
            cap_hour: 20,
            cap_day: 20
        })
    );
}

#[test]
fn an_off_list_cap_or_model_is_offered_first() {
    let mut odd = info();
    odd.wake.cap_hour = 7;
    odd.model = Some("haiku".into());
    let mut settings = settings_with(odd);
    let outcome = settings.apply(
        row_index(CoordinatorSettingsRow::CapHour, false),
        false,
        false,
    );
    assert_eq!(outcome.select, Some(0));
    assert_eq!(
        settings.picker,
        Some(CoordinatorPicker::CapHour(vec![7, 4, 8, 12, 20, 30]))
    );
    settings.close_picker(false);
    let outcome = settings.apply(
        row_index(CoordinatorSettingsRow::Model, false),
        false,
        false,
    );
    assert_eq!(outcome.select, Some(0));
    assert_eq!(
        settings.picker,
        Some(CoordinatorPicker::Model(vec![
            Some("haiku".into()),
            None,
            Some("opus".into()),
            Some("sonnet".into())
        ]))
    );
}

#[test]
fn settings_refuse_actions_while_off_and_without_a_dashboard() {
    let mut off = info();
    off.enabled = false;
    off.dashboard_url = None;
    let mut settings = settings_with(off);
    for row in [
        CoordinatorSettingsRow::WakeNow,
        CoordinatorSettingsRow::Restart,
        CoordinatorSettingsRow::OpenDashboard,
    ] {
        assert_eq!(
            settings.apply(row_index(row, false), false, false).effect,
            CoordinatorEffect::Refused("coordinator off".into()),
            "{row:?}"
        );
    }
    // Without a record nothing acts.
    let mut empty = ClientCoordinatorSettings::default();
    assert_eq!(empty.row_count(false), 0);
    assert_eq!(
        empty.apply(0, false, false).effect,
        CoordinatorEffect::Nothing
    );
}

#[test]
fn the_coordinator_keeps_its_pinned_row_first_when_space_runs_short() {
    let browser = super::super::browser::BrowserRow {
        state: super::super::browser::BrowserRowState::Running,
        status: "1 tab".into(),
        active: false,
        profile: "main".into(),
        running: true,
    };
    let news = super::super::news::NewsRow {
        tab_id: None,
        state: super::super::news::NewsRowState::Idle,
        status: "next 13:00".into(),
        focused: false,
    };
    let coordinator = CoordinatorRow {
        tab_id: None,
        state: CoordinatorRowState::Idle,
        status: "0 watched".into(),
        focused: false,
    };
    let rows = || {
        vec![
            FixedRow::Browser(&browser),
            FixedRow::News(&news),
            FixedRow::Coordinator(&coordinator),
        ]
    };
    let kinds = |rows: Vec<FixedRow<'_>>| {
        rows.iter()
            .map(|row| match row {
                FixedRow::Browser(_) => "browser",
                FixedRow::News(_) => "news",
                FixedRow::Coordinator(_) => "coordinator",
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        kinds(fixed_rows_that_fit(rows(), 3)),
        ["browser", "news", "coordinator"]
    );
    assert_eq!(
        kinds(fixed_rows_that_fit(rows(), 2)),
        ["news", "coordinator"]
    );
    assert_eq!(kinds(fixed_rows_that_fit(rows(), 1)), ["coordinator"]);
    assert!(fixed_rows_that_fit(rows(), 0).is_empty());
    // Without the coordinator, News still beats Browser.
    assert_eq!(
        kinds(fixed_rows_that_fit(
            vec![FixedRow::Browser(&browser), FixedRow::News(&news)],
            1
        )),
        ["news"]
    );
}

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config
}

fn buffer_row(buffer: &ratatui::buffer::Buffer, rect: Rect) -> String {
    (rect.x..rect.right())
        .map(|x| buffer[(x, rect.y)].symbol())
        .collect()
}

/// Render the `tabs` sidebar with the coordinator's row and a News row
/// naming `news_tab`.
fn render_sidebar(
    shell: &mut ClientShellState,
    snapshot: &ClientShellSnapshot,
    coordinator: &ClientCoordinatorState,
    news_tab: Option<&str>,
    height: u16,
) -> (ratatui::buffer::Buffer, Rect, ShellHitMap) {
    let area = Rect::new(0, 0, 32, height);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    let mut hits = ShellHitMap::default();
    let row = coordinator.row(snapshot);
    let news_row = news_tab.map(|tab_id| super::super::news::NewsRow {
        tab_id: Some(tab_id.into()),
        state: super::super::news::NewsRowState::Idle,
        status: "next 13:00".into(),
        focused: false,
    });
    let mut render_state = super::super::render::ShellRenderState {
        machine_diagnostics: &shell.machine_diagnostics,
        endpoints: &shell.endpoints,
        active_endpoint_id: &shell.active_endpoint_id,
        collapsed_endpoints: &shell.collapsed_endpoints,
        collapsed_groups: &shell.collapsed_groups,
        expanded_runs: &shell.expanded_runs,
        breathe_phase: 0.0,
        breathe_reset_rgb: None,
        idle_reminders: &shell.idle_reminders,
        scheduled_reminders: &shell.scheduled_reminders,
        news_row,
        browser_row: None,
        browser_marked_tabs: HashSet::new(),
        voice: None,
        coordinator_row: None,
        teams: None,
        remote_collapsed_groups: &shell.remote_collapsed_groups,
        workspace_scroll: &mut shell.workspace_scroll,
        agent_scroll: &mut shell.agent_scroll,
        tab_scroll: &mut shell.tab_scroll,
        reveal_focused_workspace: &mut shell.reveal_focused_workspace,
        reveal_focused_tab: &mut shell.reveal_focused_tab,
        sidebar_collapsed: false,
        sidebar_section_split: shell.sidebar_section_split,
        tab_drag_insert_index: None,
        selected_workspace_id: None,
        reveal_navigation_workspace: &mut shell.reveal_navigation_workspace,
        dragged_workspace_id: None,
        workspace_drop_indicator_row: None,
        sidebar_tab_drop_row: None,
        sidebar_model: None,
        sidebar_hover: None,
        now: std::time::Instant::now(),
        sidebar_reveal_tab: &mut shell.sidebar_reveal_tab,
        active_view: super::super::sidebar_model::ActiveView::default(),
        pins_view: super::super::sidebar_model::PinsView::default(),
        scheduled_view: super::super::sidebar_model::ScheduledView::default(),
        tab_pins: None,
        tab_mutes: None,
    };
    let rect = render_tab_sidebar_with(
        &mut buffer,
        area,
        snapshot,
        &shell.config,
        &mut render_state,
        &mut hits,
        TabSidebarCoordinator {
            row: row.as_ref(),
            teams: None,
        },
    );
    (buffer, rect, hits)
}

#[test]
fn the_tabs_sidebar_pins_the_coordinator_below_news_and_marks_no_tab() {
    let mut snapshot = coordinator_snapshot();
    snapshot.tabs.push(tab("tab_n", "ws_1", "News", false));
    let mut shell = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    shell.set_snapshot(Box::new(snapshot.clone()));
    let mut info = info();
    info.unread_suggestions = 2;
    let coordinator = state_with(info, &snapshot);

    let (buffer, rect, hits) =
        render_sidebar(&mut shell, &snapshot, &coordinator, Some("tab_n"), 20);
    assert_eq!(rect.height, 1, "coordinator row drawn");
    assert_eq!(rect.y, hits.news_row.y + 1, "directly below News");
    let text = buffer_row(&buffer, rect);
    assert!(text.contains("● coordinator"), "{text:?}");
    assert!(text.trim_end().ends_with("2 ideas"), "{text:?}");

    // Every pinned tab leaves the list.
    let listed: Vec<&str> = hits
        .sidebar_tabs
        .iter()
        .map(|(_, tab_id)| tab_id.as_str())
        .collect();
    assert_eq!(listed, ["tab_1", "tab_2", "tab_3"]);

    // Every tab is part of herdr+: the watched agents' tabs carry no `+`.
    for (rect, tab_id) in &hits.sidebar_tabs {
        let text = buffer_row(&buffer, *rect);
        assert!(!text.contains(" + "), "{tab_id}: {text:?}");
    }

    // Short on room: the coordinator keeps its row, News gives way.
    let (_, rect, hits) = render_sidebar(&mut shell, &snapshot, &coordinator, Some("tab_n"), 4);
    assert_eq!(rect.height, 1);
    assert_eq!(hits.news_row, Rect::default());
}

#[test]
fn without_the_coordinator_the_sidebar_is_unchanged() {
    let snapshot = coordinator_snapshot();
    let mut shell = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    shell.set_snapshot(Box::new(snapshot.clone()));
    let coordinator = ClientCoordinatorState::default();
    let (_, rect, hits) = render_sidebar(&mut shell, &snapshot, &coordinator, None, 20);
    assert_eq!(rect, Rect::default());
    assert_eq!(hits.sidebar_tabs.len(), 4, "the coordinator tab is listed");
}

// ----- the shell wiring (`coordinator_shell.rs`) -----------------------------

fn tabs_shell() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(coordinator_snapshot()));
    state.set_pane_surface(surface());
    state
}

fn shell_requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. }
                if crate::api::api_method_name(&request.method).starts_with("coordinator.")
                    || matches!(request.method, Method::TabFocus(_)) =>
            {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn the_shell_pulls_pins_and_focuses_the_coordinator_row() {
    let mut state = tabs_shell();
    state.compose(106, 24).expect("composed frame");
    assert_eq!(
        state.hits.coordinator_row,
        Rect::default(),
        "no reply, no row"
    );

    let mut outcome = ClientShellInput::default();
    state.tick_coordinator(std::time::Instant::now(), &mut outcome);
    let requests = shell_requests(&outcome);
    let [(request_id, Method::CoordinatorGet(_))] = &requests[..] else {
        panic!("expected one coordinator.get, got {requests:?}");
    };
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::CoordinatorGet { info: info() }),
    );
    assert!(repaint);
    let mut again = ClientShellInput::default();
    state.tick_coordinator(std::time::Instant::now(), &mut again);
    assert!(shell_requests(&again).is_empty(), "nothing changed");

    state.compose(106, 24).expect("composed frame");
    assert_ne!(state.hits.coordinator_row, Rect::default());
    let listed: Vec<&str> = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(_, id)| id.as_str())
        .collect();
    assert_eq!(
        listed,
        ["tab_1", "tab_2", "tab_3"],
        "the coordinator is pinned"
    );
    let snapshot = state.snapshot.as_deref().expect("snapshot").clone();
    let numbered: Vec<&str> = state
        .keyboard_tab_list(&snapshot, "ws_1")
        .iter()
        .map(|tab| tab.tab_id.as_str())
        .collect();
    assert_eq!(
        numbered,
        ["tab_1", "tab_2", "tab_3"],
        "keyboard numbering too"
    );

    let mut click = ClientShellInput::default();
    state.activate_coordinator_row(&mut click);
    assert!(matches!(
        &shell_requests(&click)[..],
        [(_, Method::TabFocus(target))] if target.tab_id == "tab_c"
    ));

    // A menu item reaches the server as its coordinator.* method.
    let mut wake = ClientShellInput::default();
    state.activate_coordinator_context_action(
        ClientContextMenuAction::Coordinator(CoordinatorMenuAction::WakeNow),
        &mut wake,
    );
    assert!(matches!(
        &shell_requests(&wake)[..],
        [(_, Method::CoordinatorWake(params))] if params.caller_pane.is_none()
    ));
}

#[test]
fn a_remote_dashboard_reply_shows_its_url() {
    let mut state = tabs_shell();
    let mut outcome = ClientShellInput::default();
    state.tick_coordinator(std::time::Instant::now(), &mut outcome);
    let requests = shell_requests(&outcome);
    let (_, _) = state.handle_endpoint_result(
        "boot-1",
        &requests[0].0,
        Ok(ResponseResult::CoordinatorGet { info: info() }),
    );
    let mut open = ClientShellInput::default();
    state.push_endpoint_method_with_kind(
        Method::CoordinatorOpenDashboard(
            crate::api::schema::coordinator::CoordinatorOpenDashboardParams { open: false },
        ),
        PendingEndpointKind::Coordinator(CoordinatorRequestKind::OpenDashboard { open: false }),
        &mut open,
    );
    let requests = shell_requests(&open);
    let (_, _) = state.handle_endpoint_result(
        "boot-1",
        &requests[0].0,
        Ok(ResponseResult::CoordinatorGet { info: info() }),
    );
    assert!(state
        .endpoint_error
        .as_deref()
        .is_some_and(|text| text.contains("http://127.0.0.1:7718/")));
}
