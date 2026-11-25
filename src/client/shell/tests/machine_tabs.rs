//! Fork (machine tabs): the `tabs` sidebar with more than one endpoint.
//!
//! Real servers number their spaces, tabs and panes alike (`w1:t1` …), so
//! both machines of the fixture use the same ids on purpose; only the boot
//! id and the labels differ.

use super::*;
use crate::api::schema::{AgentStatus, Method};
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crate::config::SidebarLayoutConfig;
use crate::server::headless::quiet::{QuietPayload, QuietPendingPane};
use crate::server::headless::tab_pins::TabPinsPayload;
use crate::server::headless::teams::TeamsPayload;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

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
    assert!(cached[0].contains("↻ Build"), "{cached:?}");
    let without_row = |rows: &[String]| rows[1..].to_vec();
    assert_eq!(
        without_row(&cached),
        without_row(&rows),
        "drawn from the cached snapshot"
    );
    assert!(
        !dimmed(&offline, Rect::new(1, 0, 10, 1)),
        "the machine row stays plain"
    );
    let rows = cached;
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
    assert!(one.hits.machines.is_empty(), "no machine row");
    let mut two = two_machines(SidebarLayoutConfig::Tabs);
    let shared = sidebar(&mut two).1;
    assert!(shared[0].contains("Local"), "{shared:?}");
    assert!(shared[1].contains("──"), "{shared:?}");
    // The toolbar and the list heads move down two rows, unchanged.
    assert_eq!(shared[2..10], rows[..8]);
    // A disabled machine is no second one.
    let mut off = profile();
    off.enabled = false;
    two.set_endpoint_catalog(&[off]);
    assert_eq!(sidebar(&mut two).1, rows);
    assert!(two.hits.machines.is_empty());
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

/// The machine row and its rule come off the top of the list, nothing else
/// moves; on a short window the rule goes before the row.
#[test]
fn the_layout_keeps_the_machine_row_at_the_top() {
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
    assert!(without.machines.is_empty() && without.machines_rule.is_empty());
    assert_eq!(without.toolbar.y, 3);
    let with = plan(30, 2);
    assert_eq!(with.machines, Rect::new(0, 3, 28, 1));
    assert_eq!(with.machines_rule, Rect::new(0, 4, 28, 1));
    assert_eq!(with.toolbar, Rect::new(0, 5, 28, 1));
    assert_eq!(with.list.y, 6);
    assert_eq!(with.list.height, without.list.height - 2);
    assert_eq!(with.fixed, without.fixed);
    assert_eq!(with.menu, without.menu);
    // Rule, then row; the toolbar, one list row and the menu row win.
    let heights: Vec<(u16, u16, u16)> = (0..=9)
        .map(|height| {
            let plan = plan(height, 2);
            (
                plan.machines.height,
                plan.machines_rule.height,
                plan.list.height,
            )
        })
        .collect();
    assert!(
        heights.iter().all(|(row, rule, _)| row >= rule),
        "{heights:?}"
    );
    assert_eq!(heights[3].0, 0);
    assert_eq!(heights[4], (1, 0, 1));
    assert_eq!(heights[8], (1, 0, 3), "no rule at the cost of a short list");
    assert_eq!(plan(10, 2).machines_rule.height, 1);
    for height in 0..=3 {
        assert_eq!(plan(height, 2), plan(height, 0), "{height}");
    }
}

// ----------------------------------------------------------- the machine row

fn machine(number: u8, label: &str) -> SavedSshEndpoint {
    SavedSshEndpoint {
        id: ProfileId::parse(format!("{number:032x}")).unwrap(),
        label: label.into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    }
}

fn agents(statuses: &[AgentStatus]) -> Vec<ClientShellAgent> {
    statuses
        .iter()
        .enumerate()
        .map(|(index, status)| ClientShellAgent {
            pane_id: format!("pane_{index}"),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: None,
            display_agent: None,
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: *status,
            state_change_seq: index as u64 + 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
            subagents: 0,
        })
        .collect()
}

/// A snapshot of `endpoint_id` with `statuses`, as `revision`.
fn report(
    state: &mut ClientShellState,
    endpoint_id: &ClientEndpointId,
    revision: u64,
    statuses: &[AgentStatus],
) {
    let boot = if endpoint_id.is_local() {
        LOCAL_BOOT.to_owned()
    } else {
        format!("boot of {}", endpoint_id.storage_key())
    };
    let mut snapshot = machine_snapshot(&boot, "x");
    snapshot.revision = revision;
    snapshot.agents = agents(statuses);
    state.set_endpoint_status(endpoint_id, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot(endpoint_id, Box::new(snapshot));
}

/// Local and `labels`, all online and idle, at a `width` cells wide sidebar.
fn machines(labels: &[&str], width: u16) -> (ClientShellState, Vec<ClientEndpointId>) {
    let mut state = one_machine(SidebarLayoutConfig::Tabs);
    state.sidebar_width = width;
    let profiles: Vec<SavedSshEndpoint> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| machine(index as u8 + 1, label))
        .collect();
    state.set_endpoint_catalog(&profiles);
    let mut ids = vec![ClientEndpointId::Local];
    for profile in profiles {
        let id = ClientEndpointId::Ssh(profile.id);
        report(&mut state, &id, 1, &[]);
        ids.push(id);
    }
    (state, ids)
}

fn machine_row(state: &mut ClientShellState) -> String {
    sidebar(state).1[0].trim_end_matches(['│', ' ']).to_owned()
}

fn cell(frame: &FrameData, x: u16, y: u16) -> ratatui::buffer::Cell {
    frame.to_ratatui_buffer().expect("frame")[(x, y)].clone()
}

fn machine_hit(state: &ClientShellState, id: &ClientEndpointId) -> Rect {
    state
        .hits
        .machines
        .iter()
        .find(|hit| &hit.endpoint_id == id)
        .map(|hit| hit.rect)
        .expect("in the row")
}

#[test]
fn the_row_shows_every_machines_state_label_and_count() {
    use AgentStatus::{Blocked, Done, Idle, Suspended, Working};
    let (mut state, ids) = machines(&["Build"], 36);
    let palette = state.config.palette.clone();
    assert_eq!(machine_row(&mut state), " ○ Local  ○ Build");
    for (statuses, text, color) in [
        (
            &[Working, Blocked, Working, Blocked][..],
            "◉ Build 2",
            palette.red,
        ),
        (
            &[Working, Idle, Working, Working][..],
            "● Build 3",
            palette.yellow,
        ),
        (&[Idle, Done, Suspended][..], "○ Build", palette.overlay0),
        (&[][..], "○ Build", palette.overlay0),
    ] {
        report(&mut state, &ids[1], 2, statuses);
        let (frame, rows) = sidebar(&mut state);
        assert_eq!(
            rows[0].trim_end_matches(['│', ' ']),
            format!(" ○ Local  {text}")
        );
        let rect = machine_hit(&state, &ids[1]);
        assert_eq!(cell(&frame, rect.x, 0).fg, color, "{text}");
        assert_eq!(rect.width as usize, text.chars().count());
    }
    // The machine shown: bold and underlined; the other not.
    let (frame, _) = sidebar(&mut state);
    let shown = cell(&frame, machine_hit(&state, &ids[0]).x + 2, 0);
    assert!(shown
        .modifier
        .contains(Modifier::BOLD | Modifier::UNDERLINED));
    assert_eq!(shown.fg, palette.text);
    let other = cell(&frame, machine_hit(&state, &ids[1]).x + 2, 0);
    assert!(!other
        .modifier
        .intersects(Modifier::BOLD | Modifier::UNDERLINED));
    assert_eq!(other.fg, palette.subtext0);
    // The rule under it, in the chrome's rule color.
    let rule = cell(&frame, 3, 1);
    assert_eq!((rule.symbol(), rule.fg), ("─", palette.surface1));
    // `ui.status_indicators = "symbols"`.
    state.config.status_indicators = crate::config::StatusIndicatorStyle::Symbols;
    report(&mut state, &ids[1], 3, &[Blocked]);
    assert_eq!(machine_row(&mut state), " ○ Local  × Build 1");
    // Not connected: no count of what it was doing.
    state.config.status_indicators = crate::config::StatusIndicatorStyle::Dots;
    for (status, glyph, color) in [
        (ClientEndpointStatus::Reconnecting, "↻", palette.overlay0),
        (ClientEndpointStatus::Connecting, "↻", palette.overlay0),
        (ClientEndpointStatus::Attention, "!", palette.red),
    ] {
        state.set_endpoint_status(&ids[1], status);
        let (frame, rows) = sidebar(&mut state);
        assert_eq!(
            rows[0].trim_end_matches(['│', ' ']),
            format!(" ○ Local  {glyph} Build")
        );
        let rect = machine_hit(&state, &ids[1]);
        assert_eq!(cell(&frame, rect.x, 0).fg, color);
    }
    // A login it waits for, while it retries.
    state.set_endpoint_status(&ids[1], ClientEndpointStatus::Reconnecting);
    state.set_machine_diagnostic(&ids[1], "Permission denied (publickey).".into());
    assert_eq!(machine_row(&mut state), " ○ Local  ! Build");
}

#[test]
fn the_row_fits_eighteen_to_thirty_six_columns() {
    use AgentStatus::{Blocked, Working};
    let labels = ["buildbox", "workstation"];
    let cases = [
        (36, 2, 0, " ● Local 2  ◉ buildbox 1"),
        (36, 3, 0, " ● Local 2  ◉ build… 1  ◉ works… 1"),
        (36, 3, 2, " ● Lo… 2  ◉ bu… 1  ◉ workstation 1"),
        (26, 2, 0, " ● Local 2  ◉ buildbox 1"),
        (26, 3, 0, " ● Local 2  ◉ bu…  ◉ wo…"),
        (18, 2, 0, " ● Local 2  ◉ b…"),
        (18, 2, 1, " ● L… ◉ build… 1"),
        (18, 3, 0, " ● Lo… ◉ b… ◉ w…"),
        (18, 3, 1, " ● L… ◉ bu… ◉ w…"),
    ];
    let mut rows = Vec::new();
    for (width, count, shown, _) in cases {
        let (mut state, ids) = machines(&labels[..count - 1], width);
        report(&mut state, &ids[0], 2, &[Working, Working]);
        let mut surface = boot_surface(LOCAL_BOOT);
        surface.projection_revision = 2;
        state.set_pane_surface(surface);
        for id in &ids[1..] {
            report(&mut state, id, 2, &[Blocked]);
        }
        if shown > 0 {
            assert!(state.activate_endpoint_projection(&ids[shown]));
            let boot = state.snapshot.as_ref().unwrap().boot_id.clone();
            let mut surface = boot_surface(&boot);
            surface.projection_revision = 2;
            state.set_pane_surface(surface);
        }
        rows.push(machine_row(&mut state));
        assert_eq!(state.layout(COLS, ROWS).sidebar.width, width);
        assert_eq!(state.hits.machines.len(), count, "no glyph is dropped");
    }
    let expected: Vec<&str> = cases.iter().map(|case| case.3).collect();
    assert_eq!(rows, expected);
    // One machine: no row at any width.
    for width in [18, 26, 36] {
        let (mut state, _) = machines(&[], width);
        sidebar(&mut state);
        assert!(state.hits.machines.is_empty());
    }
    // Far more than fit: the one shown is there, whichever it is.
    let labels = [
        "one", "two", "three", "four", "five", "six", "seven", "eight",
    ];
    for shown in 0..=labels.len() {
        let (mut state, ids) = machines(&labels, 18);
        if shown > 0 {
            assert!(state.activate_endpoint_projection(&ids[shown]));
            let boot = state.snapshot.as_ref().unwrap().boot_id.clone();
            state.set_pane_surface(boot_surface(&boot));
        }
        sidebar(&mut state);
        machine_hit(&state, &ids[shown]);
        assert!(state
            .hits
            .machines
            .iter()
            .all(|hit| hit.rect.right() < state.layout(COLS, ROWS).sidebar.right()));
    }
}

#[test]
fn a_click_on_a_machine_switches_retries_and_never_collapses() {
    let (mut state, ids) = machines(&["Build", "Lab"], 36);
    state.compose(COLS, ROWS).unwrap();
    let press = |state: &mut ClientShellState, id: &ClientEndpointId| {
        let rect = machine_hit(state, id);
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })])
    };
    // Another machine, and the one shown (which the runtime leaves alone
    // unless a switch away from it is under way).
    for id in [&ids[1], &ids[0]] {
        let outcome = press(&mut state, id);
        assert!(
            matches!(outcome.actions.as_slice(), [ClientShellAction::ActivateEndpoint {
                endpoint_id, target: None,
            }] if endpoint_id == id),
            "{:?}",
            outcome.actions
        );
        assert!(state.collapsed_endpoints.is_empty());
    }
    // Not connected: asked to reconnect now, like upstream's list.
    state.set_endpoint_status(&ids[2], ClientEndpointStatus::Attention);
    state.compose(COLS, ROWS).unwrap();
    let outcome = press(&mut state, &ids[2]);
    assert!(outcome.actions.is_empty() && outcome.repaint);
    assert_eq!(state.take_machine_retries(), [ids[2].clone()]);
    assert!(state.collapsed_endpoints.is_empty());
    // The one shown, lost: likewise.
    assert!(state.activate_endpoint_projection(&ids[1]));
    let boot = state.snapshot.as_ref().unwrap().boot_id.clone();
    state.set_pane_surface(boot_surface(&boot));
    state.mark_endpoint_disconnected(&ids[1]);
    state.compose(COLS, ROWS).unwrap();
    let outcome = press(&mut state, &ids[1]);
    assert!(outcome.actions.is_empty());
    assert_eq!(state.take_machine_retries(), [ids[1].clone()]);
    assert!(state.endpoint_is_active(&ids[1]));
}

#[test]
fn the_hovered_machine_has_the_hover_band() {
    let (mut state, ids) = machines(&["Build"], 36);
    state.compose(COLS, ROWS).unwrap();
    let rect = machine_hit(&state, &ids[1]);
    let moved = |state: &mut ClientShellState, x: u16, y: u16| {
        state
            .handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: y,
                modifiers: KeyModifiers::empty(),
            })])
            .repaint
    };
    assert!(moved(&mut state, rect.x, 0));
    assert!(!moved(&mut state, rect.x + 1, 0), "the same machine");
    let (frame, _) = sidebar(&mut state);
    let band = state.config.palette.sidebar_hover_bg();
    assert_eq!(cell(&frame, rect.x, 0).bg, band);
    let shown = machine_hit(&state, &ids[0]);
    assert_ne!(cell(&frame, shown.x, 0).bg, band);
    assert!(moved(&mut state, rect.x, 20));
}

/// A machine that is not shown draws its mark, nothing else.
#[test]
fn a_machine_not_shown_repaints_for_its_mark_only() {
    use AgentStatus::{Blocked, Idle, Working};
    let (mut state, ids) = machines(&["Build"], 36);
    state.compose(COLS, ROWS).unwrap();
    let mut revision = 1;
    let mut due = |state: &mut ClientShellState, statuses: &[AgentStatus]| {
        revision += 1;
        let before = state.machine_repaint_basis(&ids[1]);
        assert!(before.is_some());
        report(state, &ids[1], revision, statuses);
        state.machine_repaint_due(&ids[1], before)
    };
    assert!(!due(&mut state, &[Idle]), "idle stays idle");
    assert!(due(&mut state, &[Working]));
    assert!(!due(&mut state, &[Working, Idle]), "one is still working");
    assert!(due(&mut state, &[Working, Working]), "the count");
    assert!(due(&mut state, &[Working, Blocked]), "one waits");
    assert!(
        !due(&mut state, &[Idle, Blocked]),
        "hidden behind the waiting one"
    );
    assert!(due(&mut state, &[]));
    // Another server under the same machine (its agent cards go).
    let before = state.machine_repaint_basis(&ids[1]);
    let mut rebooted = machine_snapshot("another boot", "x");
    rebooted.revision = 1;
    state.cache_endpoint_snapshot(&ids[1], Box::new(rebooted));
    assert!(state.machine_repaint_due(&ids[1], before));
    // Where more of it shows, every snapshot repaints.
    assert!(
        state.machine_repaint_basis(&ids[0]).is_none(),
        "the one shown"
    );
    state.sidebar_collapsed = true;
    assert!(state.machine_repaint_basis(&ids[1]).is_none(), "the rail");
    state.sidebar_collapsed = false;
    state.open_navigator_overlay();
    assert!(
        state.machine_repaint_basis(&ids[1]).is_none(),
        "goto lists it"
    );
    let mut spaces = two_machines(SidebarLayoutConfig::Spaces);
    spaces.compose(COLS, ROWS).unwrap();
    assert!(spaces.machine_repaint_basis(&remote_id()).is_none());
}

#[test]
fn the_collapsed_rail_has_the_rows_glyphs() {
    let (mut state, ids) = machines(&["Build"], 36);
    report(&mut state, &ids[1], 2, &[AgentStatus::Blocked]);
    state.sidebar_collapsed = true;
    let frame = state.compose(COLS, ROWS).expect("frame").frame;
    let red = state.config.palette.red;
    let rail = machine_hit(&state, &ids[1]);
    let glyph = cell(&frame, rail.right() - 1, rail.y);
    assert_eq!((glyph.symbol(), glyph.fg), ("◉", red));
    let local = machine_hit(&state, &ids[0]);
    assert_eq!(cell(&frame, local.right() - 1, local.y).symbol(), "○");
    // The spaces layout's rail is upstream's: no glyph on Local.
    let mut spaces = two_machines(SidebarLayoutConfig::Spaces);
    spaces.sidebar_collapsed = true;
    let frame = spaces.compose(COLS, ROWS).expect("frame").frame;
    let local = machine_hit(&spaces, &ClientEndpointId::Local);
    assert_eq!(cell(&frame, local.right() - 1, local.y).symbol(), " ");
}

// ----------------------------------------------------- switching and memory

fn press_keys(state: &mut ClientShellState, keys: &[(KeyCode, KeyModifiers)]) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    for (code, modifiers) in keys {
        let next = state.handle_raw_events(vec![RawInputEvent::Key(
            crate::input::TerminalKey::new(*code, *modifiers),
        )]);
        outcome.actions.extend(next.actions);
    }
    outcome
}

#[test]
fn the_machine_keys_skip_offline_machines_and_wrap() {
    let next = [
        (KeyCode::Char('b'), KeyModifiers::CONTROL),
        (KeyCode::Char('m'), KeyModifiers::NONE),
    ];
    let target = |outcome: &ClientShellInput| match outcome.actions.as_slice() {
        [] => None,
        [ClientShellAction::ActivateEndpoint {
            endpoint_id,
            target: None,
        }] => Some(endpoint_id.clone()),
        other => panic!("{other:?}"),
    };
    let (mut state, ids) = machines(&["Build", "Lab", "Farm"], 36);
    state.set_endpoint_status(&ids[1], ClientEndpointStatus::Reconnecting);
    state.compose(COLS, ROWS).unwrap();
    let show = |state: &mut ClientShellState, id: &ClientEndpointId| {
        assert!(state.activate_endpoint_projection(id));
        let boot = state.snapshot.as_ref().unwrap().boot_id.clone();
        state.set_pane_surface(boot_surface(&boot));
        state.compose(COLS, ROWS).unwrap();
    };
    assert_eq!(target(&press_keys(&mut state, &next)), Some(ids[2].clone()));
    show(&mut state, &ids[3]);
    assert_eq!(target(&press_keys(&mut state, &next)), Some(ids[0].clone()));
    // Backward, unbound by default.
    let mut outcome = ClientShellInput::default();
    show(&mut state, &ids[0]);
    state.step_machine(false, &mut outcome);
    assert_eq!(target(&outcome), Some(ids[3].clone()));
    // From a machine that was lost, and with none other online.
    show(&mut state, &ids[2]);
    state.mark_endpoint_disconnected(&ids[2]);
    assert_eq!(target(&press_keys(&mut state, &next)), Some(ids[3].clone()));
    state.set_endpoint_status(&ids[3], ClientEndpointStatus::Attention);
    state.mark_endpoint_disconnected(&ClientEndpointId::Local);
    assert_eq!(target(&press_keys(&mut state, &next)), None);
    let mut one = one_machine(SidebarLayoutConfig::Tabs);
    one.compose(COLS, ROWS).unwrap();
    assert!(press_keys(&mut one, &next).actions.is_empty());
    // Listed in the keybind help like any other key.
    let keys = &state.config.keybinds;
    let help: Vec<(String, String)> =
        crate::input::keybind_help_groups(&keys.keybinds, &keys.prefix)
            .into_iter()
            .flat_map(|(_, entries)| entries)
            .map(|(key, label)| (key, label.into_owned()))
            .filter(|(_, label)| label.ends_with(" machine"))
            .collect();
    assert_eq!(
        help,
        [
            ("prefix+m".to_owned(), "next machine".to_owned()),
            ("unset".to_owned(), "previous machine".to_owned()),
        ]
    );
}

/// Every tab of `snapshot`, `focused` focused.
fn focus(snapshot: &mut ClientShellSnapshot, focused: &str) {
    snapshot.focused_tab_id = Some(focused.into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == focused;
    }
}

fn long_snapshot(boot: &str, name: &str, revision: u64, focused: &str) -> ClientShellSnapshot {
    let mut snapshot = machine_snapshot(boot, name);
    for number in 5..=80 {
        let mut tab = snapshot.tabs[1].clone();
        tab.tab_id = format!("tab_{number}");
        tab.number = number;
        tab.label = format!("{name}-{number}");
        snapshot.tabs.push(tab);
    }
    snapshot.revision = revision;
    focus(&mut snapshot, focused);
    snapshot
}

fn show_tab(
    state: &mut ClientShellState,
    endpoint_id: &ClientEndpointId,
    boot: &str,
    revision: u64,
    focused: &str,
) {
    state.set_endpoint_snapshot(
        endpoint_id,
        Box::new(long_snapshot(boot, "x", revision, focused)),
    );
    let mut surface = boot_surface(boot);
    surface.projection_revision = revision;
    state.set_pane_surface(surface);
    state.compose(COLS, ROWS).unwrap();
}

#[test]
fn every_machine_keeps_its_scroll_history_and_filters() {
    let local = ClientEndpointId::Local;
    let remote = remote_id();
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    for (revision, tab) in [(2, "tab_1"), (3, "tab_2"), (4, "tab_30")] {
        show_tab(&mut state, &local, LOCAL_BOOT, revision, tab);
    }
    state.agent_scroll = 45;
    state.compose(COLS, ROWS).unwrap();
    assert_eq!(state.agent_scroll, 45);
    assert!(!listed(&state).contains(&"tab_30"), "scrolled off");
    let (before, _) = sidebar(&mut state);
    let mut outcome = ClientShellInput::default();
    state.focus_sidebar_search(&mut outcome);
    state.sidebar_filter.recency = state.sidebar_filter.recency.step(true);
    state.sidebar_filter.set_query("x");
    let recency = state.sidebar_filter.recency;

    let go = |state: &mut ClientShellState, id: &ClientEndpointId, boot: &str, revision: u64| {
        assert!(state.activate_endpoint_projection(id));
        let mut surface = boot_surface(boot);
        surface.projection_revision = revision;
        state.set_pane_surface(surface);
        state.compose(COLS, ROWS).unwrap();
    };
    // Build starts fresh: nothing of Local's comes along.
    go(&mut state, &remote, REMOTE_BOOT, 1);
    assert!(!state.sidebar_filter.engaged());
    assert_eq!(state.tab_history.entries(), ["tab_1"]);
    assert_eq!(state.agent_scroll, 0);
    show_tab(&mut state, &remote, REMOTE_BOOT, 2, "tab_3");
    state.sidebar_filter.set_query("remote only");
    // A history jump whose request the switch cuts off.
    assert!(state.tab_history.back(|_| true).is_some());

    go(&mut state, &local, LOCAL_BOOT, 4);
    assert_eq!(state.tab_history.entries(), ["tab_1", "tab_2", "tab_30"]);
    assert_eq!(state.tab_history.focused(), Some("tab_30"));
    assert_eq!(state.sidebar_filter.query(), "x");
    assert_eq!(state.sidebar_filter.recency, recency);
    assert!(!state.sidebar_filter.focused, "the field lets go");
    state.clear_sidebar_filters(&mut outcome);
    assert_eq!(state.agent_scroll, 0, "clearing a filter heads the list");

    // Scroll: the very frame, as long as the same tab is focused.
    state.agent_scroll = 45;
    state.compose(COLS, ROWS).unwrap();
    go(&mut state, &remote, REMOTE_BOOT, 2);
    assert_eq!(state.tab_history.entries(), ["tab_1", "tab_3"]);
    assert_eq!(state.tab_history.focused(), Some("tab_3"));
    assert_eq!(state.sidebar_filter.query(), "remote only");
    // The jump that was cut off is over: this is a visit, not its landing.
    show_tab(&mut state, &remote, REMOTE_BOOT, 3, "tab_1");
    assert_eq!(state.tab_history.entries(), ["tab_1", "tab_3", "tab_1"]);
    go(&mut state, &local, LOCAL_BOOT, 4);
    assert_eq!(state.agent_scroll, 45);
    assert_eq!(sidebar(&mut state).0.cells, before.cells, "A, B, A");

    // Another client moved the focus meanwhile: it is revealed.
    go(&mut state, &remote, REMOTE_BOOT, 3);
    state.cache_endpoint_snapshot(
        &local,
        Box::new(long_snapshot(LOCAL_BOOT, "x", 5, "tab_70")),
    );
    go(&mut state, &local, LOCAL_BOOT, 5);
    assert!(listed(&state).contains(&"tab_70"));
    assert_eq!(state.tab_history.focused(), Some("tab_70"));

    // A restarted server: its ids mean nothing any more, the filter stays.
    state.sidebar_filter.set_query("kept");
    go(&mut state, &remote, REMOTE_BOOT, 3);
    state.cache_endpoint_snapshot(&local, Box::new(long_snapshot("boot-2", "x", 1, "tab_1")));
    go(&mut state, &local, "boot-2", 1);
    assert_eq!(state.tab_history.entries(), ["tab_1"]);
    assert_eq!(state.agent_scroll, 0);
    assert_eq!(state.sidebar_filter.query(), "kept");

    // A removed machine's view goes with it.
    go(&mut state, &remote, REMOTE_BOOT, 3);
    go(&mut state, &local, "boot-2", 1);
    assert!(state.machine_views.contains_key(&remote));
    state.set_endpoint_catalog(&[]);
    assert!(state.machine_views.is_empty());
}

/// Local shown again after the machine shown was removed.
#[test]
fn local_gets_its_view_back_when_the_shown_machine_is_removed() {
    let mut state = two_machines(SidebarLayoutConfig::Tabs);
    show_tab(&mut state, &ClientEndpointId::Local, LOCAL_BOOT, 2, "tab_1");
    show_tab(&mut state, &ClientEndpointId::Local, LOCAL_BOOT, 3, "tab_2");
    state.sidebar_filter.set_query("mine");
    switch(&mut state, &remote_id());
    state.sidebar_filter.set_query("theirs");
    state.set_endpoint_catalog(&[]);
    assert!(state.endpoint_is_active(&ClientEndpointId::Local));
    assert!(!state.sidebar_filter.engaged());
    assert!(state.activate_endpoint_projection(&ClientEndpointId::Local));
    assert_eq!(state.sidebar_filter.query(), "mine");
    assert_eq!(state.tab_history.entries(), ["tab_1", "tab_2"]);
}

/// The frames of /private/tmp/mt-t1/row-preview.txt:
/// `cargo nextest run --bin herdr machine_row_preview --run-ignored only --no-capture`
#[test]
#[ignore = "prints the machine row's frames"]
fn machine_row_preview() {
    use AgentStatus::{Blocked, Idle, Working};
    println!("colors are not in this text: a waiting machine's ◉ is red, a working one's ●");
    println!("yellow, an idle one's ○ dim; the label of the machine shown is bold and");
    println!("underlined. Local works (2 agents), buildbox waits on the user (1).\n");
    let frame = |title: &str, state: &mut ClientShellState| {
        let frame = state.compose(100, 24).expect("frame").frame;
        println!("== {title}");
        for row in frame_rows(&frame) {
            println!("{}", row.trim_end());
        }
        println!();
    };
    let setup = |labels: &[&str], width: u16| {
        let (mut state, ids) = machines(labels, width);
        report(&mut state, &ids[0], 2, &[Working, Working, Idle]);
        let mut surface = boot_surface(LOCAL_BOOT);
        surface.projection_revision = 2;
        state.set_pane_surface(surface);
        report(&mut state, &ids[1], 2, &[Blocked, Working]);
        (state, ids)
    };
    let (mut state, _) = setup(&["buildbox"], 36);
    frame("two machines, buildbox waits (36 cols)", &mut state);
    let (mut state, ids) = setup(&["buildbox", "workstation"], 36);
    report(&mut state, &ids[2], 2, &[Working]);
    frame("three machines (36 cols)", &mut state);
    let (mut state, ids) = setup(&["buildbox"], 36);
    state.set_endpoint_status(&ids[1], ClientEndpointStatus::Reconnecting);
    frame("buildbox offline (36 cols)", &mut state);
    let (mut state, ids) = setup(&["buildbox", "workstation"], 18);
    report(&mut state, &ids[2], 2, &[Working]);
    frame("three machines (18 cols)", &mut state);
    let (mut state, _) = setup(&["buildbox"], 18);
    frame("two machines, buildbox waits (18 cols)", &mut state);
}

/// One frame with one machine and with two, and what a snapshot of the
/// machine not shown costs, as its tabs grow:
/// `cargo test --release --bin herdr machine_tabs_render_scale_profile -- --ignored --nocapture`
#[test]
#[ignore = "manual machine tabs composition scaling profile"]
fn machine_tabs_render_scale_profile() {
    const FRAMES: u32 = 2000;
    fn grown(boot: &str, name: &str, tabs: usize) -> ClientShellSnapshot {
        let mut snapshot = machine_snapshot(boot, name);
        let tab = snapshot.tabs[0].clone();
        snapshot.tabs = (1..=tabs)
            .map(|number| {
                let mut tab = tab.clone();
                tab.tab_id = format!("tab_{number}");
                tab.number = number;
                tab.focused = number == 1;
                tab
            })
            .collect();
        snapshot.agents = (1..=tabs)
            .map(|number| {
                let mut agent = ClientShellState::test_one_agent_snapshot(
                    boot,
                    1,
                    crate::api::schema::AgentStatus::Working,
                )
                .agents[0]
                    .clone();
                agent.pane_id = format!("pane_{number}");
                agent.tab_id = format!("tab_{number}");
                agent
            })
            .collect();
        snapshot
    }
    let us = |start: std::time::Instant| start.elapsed().as_secs_f64() * 1e6 / f64::from(FRAMES);
    for tabs in [1, 15, 50] {
        for machines in [1, 2] {
            let mut state = if machines == 1 {
                one_machine(SidebarLayoutConfig::Tabs)
            } else {
                two_machines(SidebarLayoutConfig::Tabs)
            };
            let mut local = grown(LOCAL_BOOT, "local", tabs);
            local.revision = 2;
            state.set_endpoint_snapshot(&ClientEndpointId::Local, Box::new(local));
            let mut surface = boot_surface(LOCAL_BOOT);
            surface.projection_revision = 2;
            state.set_pane_surface(surface);
            for _ in 0..50 {
                std::hint::black_box(state.compose(COLS, ROWS).expect("frame"));
            }
            let start = std::time::Instant::now();
            for _ in 0..FRAMES {
                std::hint::black_box(state.compose(COLS, ROWS).expect("frame"));
            }
            eprintln!(
                "{tabs:>3} tabs, {machines} machine(s): {:.1} us/frame",
                us(start)
            );
            if machines == 2 {
                let hidden = grown(REMOTE_BOOT, "remote", tabs);
                let start = std::time::Instant::now();
                let mut frames = 0;
                for revision in 0..FRAMES {
                    let mut snapshot = Box::new(hidden.clone());
                    snapshot.revision = u64::from(revision) + 2;
                    let before = state.machine_repaint_basis(&remote_id());
                    state.cache_endpoint_snapshot(&remote_id(), snapshot);
                    frames += u32::from(state.machine_repaint_due(&remote_id(), before));
                }
                eprintln!(
                    "{tabs:>3} tabs, a snapshot of the machine not shown: {:.1} us ({frames} frames of {FRAMES})",
                    us(start)
                );
            }
        }
    }
}
