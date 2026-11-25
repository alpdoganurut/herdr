//! Fork (machine tabs): the `tabs` sidebar with more than one endpoint.
//!
//! Real servers number their spaces, tabs and panes alike (`w1:t1` …), so
//! both machines of the fixture use the same ids on purpose; only the boot
//! id and the labels differ.

use super::*;
use crate::api::schema::Method;
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crate::config::SidebarLayoutConfig;
use crate::server::headless::quiet::{QuietPayload, QuietPendingPane};
use crate::server::headless::tab_pins::TabPinsPayload;
use crate::server::headless::teams::TeamsPayload;
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

const COLS: u16 = 120;
const ROWS: u16 = 34;
const LOCAL_BOOT: &str = "boot-1";
const REMOTE_BOOT: &str = "remote-boot";

fn profile() -> SavedSshEndpoint {
    SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    }
}

fn remote_id() -> ClientEndpointId {
    ClientEndpointId::Ssh(profile().id)
}

/// The bucket `ws_1` and the group `ws_2`, two tabs each; `tab_1` focused.
fn machine_snapshot(boot: &str, name: &str) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.boot_id = boot.into();
    snapshot.workspaces[0].label = format!("{name}-bucket");
    let mut group = snapshot.workspaces[0].clone();
    group.workspace_id = "ws_2".into();
    group.active_tab_id = "tab_3".into();
    group.number = 2;
    group.label = format!("{name}-group");
    group.focused = false;
    snapshot.workspaces.push(group);
    let template = snapshot.tabs[0].clone();
    snapshot.tabs = (1..=4)
        .map(|number| {
            let mut tab = template.clone();
            tab.tab_id = format!("tab_{number}");
            tab.workspace_id = if number <= 2 { "ws_1" } else { "ws_2" }.into();
            tab.number = number;
            tab.label = format!("{name}-{number}");
            tab.custom_label = true;
            tab.focused = number == 1;
            tab
        })
        .collect();
    snapshot
}

fn config(layout: SidebarLayoutConfig) -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = layout;
    config.ui.sidebar_active_agents = false;
    config
}

fn boot_surface(boot: &str) -> PaneSurfaceFrame {
    let mut surface = surface();
    surface.boot_id = boot.into();
    surface
}

/// Local online and shown.
fn one_machine(layout: SidebarLayoutConfig) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config(layout)));
    let clock = std::time::Instant::now();
    state.sidebar_clock = Some(clock);
    state.breathe_clock = Some(clock);
    state.set_snapshot(Box::new(machine_snapshot(LOCAL_BOOT, "local")));
    state.set_pane_surface(boot_surface(LOCAL_BOOT));
    state
}

/// Local shown, the saved machine `Build` online beside it.
fn two_machines(layout: SidebarLayoutConfig) -> ClientShellState {
    let mut state = one_machine(layout);
    state.set_endpoint_catalog(&[profile()]);
    state.set_endpoint_status(&remote_id(), ClientEndpointStatus::Online);
    state.set_endpoint_snapshot(
        &remote_id(),
        Box::new(machine_snapshot(REMOTE_BOOT, "remote")),
    );
    state
}

fn switch(state: &mut ClientShellState, endpoint_id: &ClientEndpointId) {
    assert!(state.activate_endpoint_projection(endpoint_id));
    let boot = if endpoint_id.is_local() {
        LOCAL_BOOT
    } else {
        REMOTE_BOOT
    };
    state.set_pane_surface(boot_surface(boot));
}

/// The composed frame's rows in the sidebar column, with the pane area cut.
fn sidebar(state: &mut ClientShellState) -> (FrameData, Vec<String>) {
    let frame: FrameData = state.compose(COLS, ROWS).expect("composed frame").frame;
    let width = usize::from(state.layout(COLS, ROWS).sidebar.width);
    assert!(width > 0);
    let rows = frame
        .cells
        .chunks(usize::from(frame.width))
        .map(|row| {
            row[..width]
                .iter()
                .map(|cell| cell.symbol.as_str())
                .collect()
        })
        .collect();
    (frame, rows)
}

fn sidebar_text(state: &mut ClientShellState) -> String {
    sidebar(state).1.join("\n")
}

fn listed(state: &ClientShellState) -> Vec<&str> {
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(_, id)| id.as_str())
        .collect()
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    let event = |kind| {
        RawInputEvent::Mouse(MouseEvent {
            kind,
            column: rect.x + 6,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    let mut outcome = state.handle_raw_events(vec![event(MouseEventKind::Down(MouseButton::Left))]);
    let released = state.handle_raw_events(vec![event(MouseEventKind::Up(MouseButton::Left))]);
    outcome.actions.extend(released.actions);
    outcome
}

fn tab_row(state: &ClientShellState, tab_id: &str) -> Rect {
    state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("{tab_id} is listed"))
}

fn group_row(state: &ClientShellState, workspace_id: &str) -> Rect {
    state
        .hits
        .sidebar_groups
        .iter()
        .find(|(_, id)| id == workspace_id)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("{workspace_id} has a header"))
}

#[test]
fn the_tabs_sidebar_draws_the_active_machine() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    let (first, text) = sidebar(&mut state);
    let text = text.join("\n");
    assert!(text.contains("local-1"), "{text}");
    assert!(text.contains("local-group"), "{text}");
    assert!(!text.contains("remote"), "{text}");
    assert!(!text.contains(" machines"), "{text}");
    assert_eq!(listed(&state), ["tab_1", "tab_2", "tab_3", "tab_4"]);

    switch(&mut state, &remote_id());
    let text = sidebar_text(&mut state);
    assert!(text.contains("remote-1"), "{text}");
    assert!(text.contains("remote-group"), "{text}");
    assert!(!text.contains("local"), "{text}");
    assert!(
        state
            .hits
            .workspaces
            .iter()
            .all(|hit| hit.endpoint_id == remote_id()),
        "the headers belong to the machine shown"
    );
    let builds = state.sidebar_model.builds;
    assert!(builds > 0, "compose drew the cached model");
    state.compose(COLS, ROWS).unwrap();
    assert_eq!(state.sidebar_model.builds, builds, "unchanged: no rebuild");

    switch(&mut state, &ClientEndpointId::Local);
    let (again, _) = sidebar(&mut state);
    assert_eq!(again.cells, first.cells, "A, B, A: the same frame");
}

#[test]
fn folds_are_kept_per_machine() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    switch(&mut state, &remote_id());
    state.compose(COLS, ROWS).unwrap();
    let header = group_row(&state, "ws_2");
    let outcome = click(&mut state, header);
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
    state.compose(COLS, ROWS).unwrap();
    assert_eq!(listed(&state), ["tab_1", "tab_2"], "folded on Build");
    assert!(state.collapsed_groups.is_empty());
    assert!(state.group_is_collapsed(&remote_id(), &group_key("ws_2")));

    switch(&mut state, &ClientEndpointId::Local);
    state.compose(COLS, ROWS).unwrap();
    assert_eq!(
        listed(&state),
        ["tab_1", "tab_2", "tab_3", "tab_4"],
        "the same id is open on Local"
    );
    // Fold all folds the machine shown only, unfold all likewise.
    let mut outcome = ClientShellInput::default();
    state.toggle_all_groups_folded(&mut outcome);
    assert!(state.group_is_collapsed(&ClientEndpointId::Local, &group_key("ws_2")));
    switch(&mut state, &remote_id());
    state.toggle_all_groups_folded(&mut outcome);
    assert!(!state.group_is_collapsed(&remote_id(), &group_key("ws_2")));
    assert!(state.group_is_collapsed(&ClientEndpointId::Local, &group_key("ws_2")));
    state.compose(COLS, ROWS).unwrap();
    assert_eq!(listed(&state), ["tab_1", "tab_2", "tab_3", "tab_4"]);
    // A jump opens the tab's group on the machine shown.
    state.toggle_all_groups_folded(&mut outcome);
    state.jump_to_sidebar_tab("tab_3".into(), &mut outcome);
    assert!(!state.group_is_collapsed(&remote_id(), &group_key("ws_2")));
    assert!(state.group_is_collapsed(&ClientEndpointId::Local, &group_key("ws_2")));
}

#[test]
fn a_tab_row_click_reaches_the_machine_shown() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    switch(&mut state, &remote_id());
    state.compose(COLS, ROWS).unwrap();
    let row = tab_row(&state, "tab_3");
    let outcome = click(&mut state, row);
    let [ClientShellAction::Endpoint {
        endpoint_id,
        boot_id,
        request,
    }] = outcome.actions.as_slice()
    else {
        panic!("one request: {:?}", outcome.actions);
    };
    assert_eq!(endpoint_id, &remote_id());
    assert_eq!(boot_id, REMOTE_BOOT);
    assert!(
        matches!(&request.method, Method::TabFocus(target) if target.tab_id == "tab_3"),
        "{:?}",
        request.method
    );
}

/// Upstream's guarantee for its machine list, kept for the tabs rows: while
/// a switch is under way Local is still drawn, and a click there cancels it.
#[test]
fn a_local_row_click_can_cancel_a_switch_under_way() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    state.compose(COLS, ROWS).unwrap();
    let mut pending = ClientShellInput::default();
    assert!(state.activate_endpoint(remote_id(), &mut pending));
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
    let row = tab_row(&state, "tab_2");
    let outcome = click(&mut state, row);
    assert!(
        matches!(outcome.actions.as_slice(), [ClientShellAction::ActivateEndpoint {
            endpoint_id: ClientEndpointId::Local,
            target: Some(ClientEndpointFocusTarget::Tab(tab)),
        }] if tab == "tab_2"),
        "{:?}",
        outcome.actions
    );
}

#[test]
fn the_list_scroll_does_not_travel_to_another_machine() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    let mut long = machine_snapshot(LOCAL_BOOT, "local");
    for number in 5..=80 {
        let mut tab = long.tabs[1].clone();
        tab.tab_id = format!("tab_{number}");
        tab.number = number;
        tab.label = format!("local-{number}");
        long.tabs.push(tab);
    }
    // Build's focused tab is in view from either scroll, so the reveal on a
    // switch leaves whatever scroll it finds.
    let mut remote = long.clone();
    remote.boot_id = REMOTE_BOOT.into();
    remote.focused_tab_id = Some("tab_22".into());
    for tab in &mut remote.tabs {
        tab.focused = tab.tab_id == "tab_22";
    }
    state.set_endpoint_snapshot(&ClientEndpointId::Local, Box::new(long));
    state.set_endpoint_snapshot(&remote_id(), Box::new(remote));
    state.set_pane_surface(boot_surface(LOCAL_BOOT));
    state.compose(COLS, ROWS).unwrap();
    assert!(state.hits.agent_max_scroll > 20);
    state.agent_scroll = 20;
    state.compose(COLS, ROWS).unwrap();
    assert_eq!(state.agent_scroll, 20);
    switch(&mut state, &remote_id());
    state.compose(COLS, ROWS).unwrap();
    assert!(
        listed(&state).contains(&"tab_22"),
        "the focused tab is shown"
    );
    assert_eq!(state.agent_scroll, 0, "from the list's top");
    // The aggregate agent panel's reveal has no list here.
    state.reveal_endpoint_agent(&remote_id(), "pane_1", 3);
    assert_eq!(state.agent_scroll, 0);
}

#[test]
fn a_lost_machine_stays_selected_and_dimmed() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    switch(&mut state, &remote_id());
    let (online, rows) = sidebar(&mut state);
    let width = state.layout(COLS, ROWS).sidebar.width;
    let dimmed = |frame: &FrameData, rect: Rect| {
        let buffer = frame.to_ratatui_buffer().expect("frame");
        (rect.x..rect.right()).all(|x| buffer[(x, rect.y)].modifier.contains(Modifier::DIM))
    };
    let row = tab_row(&state, "tab_2");
    assert!(!dimmed(&online, row));

    state.mark_endpoint_disconnected(&remote_id());
    let (offline, cached) = sidebar(&mut state);
    assert_eq!(cached, rows, "drawn from the cached snapshot");
    assert_eq!(tab_row(&state, "tab_2"), row);
    assert!(dimmed(&offline, row));
    assert!(dimmed(&offline, group_row(&state, "ws_2")));
    assert!(state.hits.panes.is_empty(), "no input into cached panes");
    assert!(offline.cursor.is_none());
    assert!(state.endpoint_is_active(&remote_id()));

    // Local keeps reporting: the lost machine stays selected.
    let mut local = machine_snapshot(LOCAL_BOOT, "local");
    local.revision = 2;
    state.set_endpoint_snapshot(&ClientEndpointId::Local, Box::new(local));
    assert!(state.endpoint_is_active(&remote_id()));
    assert_eq!(sidebar(&mut state).1, rows);

    // A resize drops the frozen surface; the sidebar stays.
    state.invalidate_pane_surface();
    let (gap, cached) = sidebar(&mut state);
    assert_eq!(cached, rows);
    assert!(dimmed(&gap, row));
    assert!(state.hits.machines.is_empty());
    let text = frame_rows(&gap).join("\n");
    assert!(text.contains("Build: reconnecting"), "{text}");
    assert_eq!(state.layout(COLS, ROWS).sidebar.width, width);

    // Back online: undimmed.
    state.set_endpoint_status(&remote_id(), ClientEndpointStatus::Online);
    state.set_pane_surface(boot_surface(REMOTE_BOOT));
    let (back, _) = sidebar(&mut state);
    assert!(!dimmed(&back, row));
}

#[test]
fn the_surface_gap_keeps_the_active_machines_sidebar() {
    for endpoint_id in [ClientEndpointId::Local, remote_id()] {
        let mut state = two_machines(SidebarLayoutConfig::Tabs);
        if !endpoint_id.is_local() {
            switch(&mut state, &endpoint_id);
        }
        let (_, full) = sidebar(&mut state);
        state.invalidate_pane_surface();
        let (gap, rows) = sidebar(&mut state);
        assert_eq!(rows, full, "{endpoint_id:?}");
        let text = frame_rows(&gap).join("\n");
        assert!(!text.contains("Select a connected machine"), "{text}");
    }
}

#[test]
fn one_machine_and_the_spaces_layout_keep_their_sidebars() {
    // One machine: same rows as before, and never dimmed.
    let mut one = one_machine(SidebarLayoutConfig::Tabs);
    let (online, rows) = sidebar(&mut one);
    let mut two = two_machines(SidebarLayoutConfig::Tabs);
    assert_eq!(sidebar(&mut two).1, rows);
    assert!(rows[1].contains("local-1"), "under the toolbar: {rows:?}");
    one.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
    let (offline, _) = sidebar(&mut one);
    let width = usize::from(one.layout(COLS, ROWS).sidebar.width);
    for (before, after) in online
        .cells
        .chunks(usize::from(COLS))
        .zip(offline.cells.chunks(usize::from(COLS)))
    {
        assert_eq!(before[..width], after[..width]);
    }

    // Spaces: upstream's machine list.
    let mut spaces = two_machines(SidebarLayoutConfig::Spaces);
    let text = sidebar_text(&mut spaces);
    assert!(text.contains(" machines"), "{text}");
    assert!(text.contains("Build"), "{text}");
    assert!(text.contains("remote-group"), "{text}");
    assert_eq!(spaces.hits.machines.len(), 2);
    assert!(spaces.hits.sidebar_tabs.is_empty());

    // Tabs, collapsed: upstream's machine rail, whose rows focus.
    let mut rail = two_machines(SidebarLayoutConfig::Tabs);
    rail.sidebar_collapsed = true;
    rail.compose(COLS, ROWS).unwrap();
    assert_eq!(rail.hits.machines.len(), 2);
    assert!(rail.hits.sidebar_tabs.is_empty());

    // Tabs on a phone: upstream's list while there is no pane surface.
    let mut phone = two_machines(SidebarLayoutConfig::Tabs);
    phone.invalidate_pane_surface();
    phone.compose(40, ROWS).unwrap();
    assert!(!phone.layout(40, ROWS).mobile_header.is_empty());
    assert_eq!(phone.hits.machines.len(), 2);
    assert!(phone.hits.sidebar_tabs.is_empty());
}

/// Both machines' pushes are kept; the sidebar shows the active one's.
#[test]
fn pushes_stay_with_their_machine() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    let local = ClientEndpointId::Local;
    let remote = remote_id();
    for (endpoint_id, boot, tab) in [
        (&local, LOCAL_BOOT, "tab_1"),
        (&remote, REMOTE_BOOT, "tab_2"),
    ] {
        state.receive_tab_pins(
            endpoint_id,
            TabPinsPayload {
                boot_id: boot.into(),
                revision: 1,
                tab_ids: vec![tab.into()],
            },
        );
        state.receive_quiet(
            endpoint_id,
            QuietPayload {
                boot_id: boot.into(),
                revision: 1,
                dnd_tab_ids: vec![tab.into()],
                pending: vec![QuietPendingPane {
                    pane_id: "pane_1".into(),
                    tab_id: tab.into(),
                    count: 2,
                }],
            },
        );
        state.receive_teams(
            endpoint_id,
            TeamsPayload {
                boot_id: boot.into(),
                revision: 1,
                teams: vec![crate::api::schema::TeamInfo {
                    workspace_id: "ws_2".into(),
                    workspace_label: "group".into(),
                    purpose: Some(boot.into()),
                    ..Default::default()
                }],
            },
        );
    }
    for (endpoint_id, boot, pinned) in [
        (local.clone(), LOCAL_BOOT, "tab_1"),
        (remote, REMOTE_BOOT, "tab_2"),
        (local, LOCAL_BOOT, "tab_1"),
    ] {
        switch(&mut state, &endpoint_id);
        let text = sidebar_text(&mut state);
        assert_eq!(
            state
                .hits
                .sidebar_pins_rows
                .iter()
                .map(|(_, id)| id.as_str())
                .collect::<Vec<_>>(),
            [pinned],
            "{endpoint_id:?}"
        );
        assert!(text.contains(boot), "{text}");
        assert!(state.active_tab_dnd(pinned));
        assert!(!state.active_tab_dnd(if pinned == "tab_1" { "tab_2" } else { "tab_1" }));
    }
}

/// Ids repeat across machines: what the pointer and the single-slot reads
/// of one machine said never lands on the other's rows.
#[test]
fn a_switch_drops_what_belonged_to_the_machine_left() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    state.compose(COLS, ROWS).unwrap();
    state.sidebar_hover = Some(super::super::sidebar_model::SidebarHover::Tab(
        "tab_2".into(),
    ));
    state.sidebar_reveal_tab = Some("tab_4".into());
    state.news.info = Some(
        serde_json::from_value(serde_json::json!({
            "enabled": true,
            "times": [],
            "quiet_hours": "",
            "tab_id": "tab_2",
            "unread": false,
            "consecutive_failures": 0,
            "pending_notifications": 0,
        }))
        .expect("news info"),
    );
    state.compose(COLS, ROWS).unwrap();
    assert_eq!(
        listed(&state),
        ["tab_1", "tab_3", "tab_4"],
        "News is pinned"
    );
    switch(&mut state, &remote_id());
    state.compose(COLS, ROWS).unwrap();
    assert!(state.sidebar_hover.is_none());
    assert!(state.sidebar_reveal_tab.is_none());
    assert!(state.news_row().is_none());
    assert_eq!(listed(&state), ["tab_1", "tab_2", "tab_3", "tab_4"]);
}

/// A poll interrupted by a switch or a lost connection is neither the
/// user's action nor an answer: no notice, the rows stay, and it asks again
/// a second later, once a second while it keeps being refused.
#[test]
fn an_interrupted_poll_is_silent_and_asks_again() {
    use std::time::{Duration, Instant};
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    state.compose(COLS, ROWS).unwrap();
    type Poll = fn(&mut ClientShellState, Instant, &mut ClientShellInput);
    let polls: [(Poll, &str); 3] = [
        (ClientShellState::tick_news, "news.get"),
        (ClientShellState::tick_coordinator, "coordinator.get"),
        (ClientShellState::tick_browser, "browser.get"),
    ];
    for (tick, name) in polls {
        let ask = |state: &mut ClientShellState, now: Instant| {
            let mut outcome = ClientShellInput::default();
            tick(state, now, &mut outcome);
            outcome.actions.iter().find_map(|action| match action {
                ClientShellAction::Endpoint { request, .. }
                    if crate::api::api_method_name(&request.method) == name =>
                {
                    Some(request.id.clone())
                }
                _ => None,
            })
        };
        let mut now = Instant::now();
        let mut request_id = ask(&mut state, now).expect("asked on attach");
        assert!(ask(&mut state, now).is_none(), "{name}: one at a time");
        for _ in 0..3 {
            state.cancel_endpoint_request(&request_id);
            assert!(state.visible_endpoint_notice.is_none(), "{name}");
            assert!(ask(&mut state, now).is_none(), "{name}: not every tick");
            now = Instant::now() + Duration::from_millis(1100);
            request_id = ask(&mut state, now).expect("asked again");
            now = Instant::now();
        }
    }
}

/// The spaces of the machine shown are what the keys step through.
#[test]
fn space_keys_stay_on_the_machine_shown() {
    use crate::input::KeybindAction;
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    switch(&mut state, &remote_id());
    let mut on_last = machine_snapshot(REMOTE_BOOT, "remote");
    on_last.revision = 2;
    on_last.focused_workspace_id = Some("ws_2".into());
    for workspace in &mut on_last.workspaces {
        workspace.focused = workspace.workspace_id == "ws_2";
    }
    state.set_endpoint_snapshot(&remote_id(), Box::new(on_last));
    let mut surface = boot_surface(REMOTE_BOOT);
    surface.projection_revision = 2;
    state.set_pane_surface(surface);
    state.compose(COLS, ROWS).unwrap();
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_endpoint_navigation(KeybindAction::NextWorkspace, &mut outcome));
    assert!(
        matches!(outcome.actions.as_slice(), [ClientShellAction::Endpoint { endpoint_id, request, .. }]
            if endpoint_id == &remote_id()
                && matches!(&request.method, Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_1")),
        "{:?}",
        outcome.actions
    );
    state.navigate_workspace_id = state.focused_navigation_target();
    for expected in ["ws_1", "ws_2", "ws_1"] {
        state.move_navigate_workspace(1);
        let target = state.navigate_workspace_id.clone().expect("a target");
        assert_eq!(target.endpoint_id, remote_id());
        assert_eq!(target.workspace_id, expected);
    }
    // The collapsed rail lists every machine, and the keys follow it.
    state.sidebar_collapsed = true;
    state.move_navigate_workspace(1);
    state.move_navigate_workspace(1);
    assert!(state
        .navigate_workspace_id
        .as_ref()
        .is_some_and(|target| target.endpoint_id.is_local()));
}

#[test]
fn the_search_field_works_on_the_machine_shown() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    switch(&mut state, &remote_id());
    state.compose(COLS, ROWS).unwrap();
    let mut outcome = ClientShellInput::default();
    state.focus_sidebar_search(&mut outcome);
    assert!(state.sidebar_search_focused());
    state.sidebar_filter.set_query("remote-4");
    state.sidebar_model.mark_dirty();
    state.compose(COLS, ROWS).unwrap();
    assert!(state.sidebar_search_focused(), "compose keeps it");
    assert_eq!(
        listed(&state),
        ["tab_1", "tab_4"],
        "the focused tab and the match"
    );
}

fn suspended_run(boot: &str, name: &str) -> ClientShellSnapshot {
    let mut snapshot = machine_snapshot(boot, name);
    for number in 5..=8 {
        let mut tab = snapshot.tabs[3].clone();
        tab.tab_id = format!("tab_{number}");
        tab.number = number;
        tab.label = format!("{name}-{number}");
        tab.agent_status = crate::api::schema::AgentStatus::Suspended;
        snapshot.tabs.push(tab);
    }
    snapshot
}

#[test]
fn expanded_suspended_runs_are_kept_per_machine_and_persist() {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "herdr-machine-tabs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let path = dir.join("local.json");
    let setup = || {
        let mut state = two_machines(SidebarLayoutConfig::Tabs);
        state.config.preferences_path = Some(path.clone());
        for (endpoint_id, boot, name) in [
            (ClientEndpointId::Local, LOCAL_BOOT, "local"),
            (remote_id(), REMOTE_BOOT, "remote"),
        ] {
            let mut snapshot = suspended_run(boot, name);
            snapshot.revision = 1;
            state.set_endpoint_snapshot(&endpoint_id, Box::new(snapshot));
        }
        state
    };
    let mut state = setup();
    switch(&mut state, &remote_id());
    state.compose(COLS, ROWS).unwrap();
    let run = state.hits.sidebar_runs[0].clone();
    assert_eq!(run.1, "tab_5");
    click(&mut state, run.0);
    state.compose(COLS, ROWS).unwrap();
    assert!(listed(&state).contains(&"tab_6"), "expanded on Build");
    switch(&mut state, &ClientEndpointId::Local);
    state.compose(COLS, ROWS).unwrap();
    assert!(!listed(&state).contains(&"tab_6"), "folded on Local");
    assert!(state.expanded_runs.is_empty());

    // Persisted by profile, beside Local's.
    let saved = super::super::preferences::load(&path).expect("preferences");
    assert!(saved.expanded_runs.is_empty());
    assert_eq!(saved.remote_expanded_runs.len(), 1);
    assert_eq!(saved.remote_expanded_runs[0].expanded_runs, ["tab_5"]);
    let restored = ClientShellState::new(
        ClientShellConfig::from_config(&config(SidebarLayoutConfig::Tabs))
            .with_preferences_path(path.clone()),
    );
    assert!(restored.expanded_runs.is_empty());
    assert!(restored.remote_expanded_runs[&remote_id()].contains("tab_5"));
    std::fs::remove_dir_all(&dir).expect("the test's own directory");
}

/// Task 2's row: its rows come off the top, nothing else moves sideways.
#[test]
fn the_layout_keeps_rows_for_the_machine_row_at_the_top() {
    use super::super::tab_sidebar::{plan_layout, LayoutWants, SectionRows};
    let none = |_cap: u16| 0;
    let plan = |height, machine_rows| {
        plan_layout(
            Rect::new(0, 3, 28, height),
            LayoutWants {
                machine_rows,
                fixed: 2,
                ..LayoutWants::default()
            },
            SectionRows::active_only(&none),
        )
    };
    let without = plan(30, 0);
    assert!(without.machines.is_empty());
    assert_eq!(without.toolbar.y, 3);
    let with = plan(30, 1);
    assert_eq!(with.machines, Rect::new(0, 3, 28, 1));
    assert_eq!(with.toolbar, Rect::new(0, 4, 28, 1));
    assert_eq!(with.list.y, 5);
    assert_eq!(with.list.height, without.list.height - 1);
    assert_eq!(with.fixed, without.fixed);
    assert_eq!(with.menu, without.menu);
    // The last list row, the toolbar and the menu row win over it.
    for height in 0..=3 {
        assert_eq!(plan(height, 1), plan(height, 0), "{height}");
    }
    assert_eq!(plan(4, 1).list.height, 1);
}
