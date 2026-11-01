//! Fork (sidebar filters): the `tabs` sidebar's search field and recency
//! chip (`sidebar_filter.rs`), at the model (`SidebarModel` rows) and at the
//! input and render level (`ClientShellState`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::super::agent_times::ClientAgentTimesState;
use super::super::sidebar_filter::{Recency, SidebarFilter};
use super::super::sidebar_model::{ModelInputs, Row, SidebarHover, SidebarModel};
use super::*;
use crate::config::{Config, SidebarLayoutConfig};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config.ui.sidebar_active_agents = false;
    config
}

fn workspace(id: &str, number: usize, label: &str, focused: bool) -> ClientShellWorkspace {
    let mut workspace = snapshot().workspaces[0].clone();
    workspace.workspace_id = id.into();
    workspace.number = number;
    workspace.label = label.into();
    workspace.focused = focused;
    workspace
}

fn tab(id: &str, workspace_id: &str, label: &str, status: AgentStatus) -> ClientShellTab {
    ClientShellTab {
        tab_id: id.into(),
        workspace_id: workspace_id.into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused: false,
        agent_status: status,
        color: None,
        important: false,
        remind_every: None,
    }
}

fn agent(pane_id: &str, tab: &ClientShellTab, kind: &str) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: tab.workspace_id.clone(),
        tab_id: tab.tab_id.clone(),
        name: None,
        display_agent: Some(kind.into()),
        agent: Some(kind.into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: tab.agent_status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }
}

/// The bucket (`lead` focused and working with claude, `reviewer` idle with
/// codex), group `api` (`notes` idle with claude, then four suspended
/// `parked N` tabs: a run), group `infra` (`deploy` done with claude).
/// Pane `pane_N` belongs to `tab_N`.
fn fixture() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.workspaces = vec![
        workspace("ws_1", 1, "client-shell", true),
        workspace("ws_2", 2, "api", false),
        workspace("ws_3", 3, "infra", false),
    ];
    let mut tabs = vec![
        tab("tab_1", "ws_1", "lead", AgentStatus::Working),
        tab("tab_2", "ws_1", "reviewer", AgentStatus::Idle),
        tab("tab_3", "ws_2", "notes", AgentStatus::Idle),
    ];
    for index in 4..=7 {
        tabs.push(tab(
            &format!("tab_{index}"),
            "ws_2",
            &format!("parked {index}"),
            AgentStatus::Suspended,
        ));
    }
    tabs.push(tab("tab_8", "ws_3", "deploy", AgentStatus::Done));
    tabs[0].focused = true;
    snapshot.panes = tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| ClientShellPane {
            pane_id: format!("pane_{}", index + 1),
            workspace_id: tab.workspace_id.clone(),
            tab_id: tab.tab_id.clone(),
            label: None,
            cwd: Some("/repo".into()),
            foreground_cwd: Some("/repo".into()),
            focused: index == 0,
            right_click_passthrough: false,
        })
        .collect();
    snapshot.agents = tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            let kind = if index == 1 { "codex" } else { "claude" };
            agent(&format!("pane_{}", index + 1), tab, kind)
        })
        .collect();
    snapshot.tabs = tabs;
    snapshot
}

/// Agent times: `pane_N` entered its state `ago[N - 1]` before `now`
/// (`None`: no time).
fn times(now: Instant, ago: &[Option<u64>]) -> ClientAgentTimesState {
    ClientAgentTimesState {
        boot_id: "boot-1".into(),
        revision: 1,
        panes: ago
            .iter()
            .enumerate()
            .filter_map(|(index, minutes)| {
                let at = now - Duration::from_secs((*minutes)? * 60);
                Some((format!("pane_{}", index + 1), (1, at)))
            })
            .collect::<HashMap<_, _>>(),
    }
}

fn query(text: &str) -> SidebarFilter {
    let mut filter = SidebarFilter::default();
    filter.set_query(text);
    filter
}

fn build(
    snapshot: &ClientShellSnapshot,
    filter: &SidebarFilter,
    times: Option<&ClientAgentTimesState>,
    now: Instant,
) -> SidebarModel {
    let collapsed = HashSet::new();
    SidebarModel::built(ModelInputs {
        filter: Some(filter),
        times,
        now: Some(now),
        ..ModelInputs::new(snapshot, &collapsed)
    })
}

/// The rows as `H:group(members)`, `T:tab`, `R:len` and `F:hidden`.
fn rows(snapshot: &ClientShellSnapshot, model: &SidebarModel) -> Vec<String> {
    model
        .rows
        .iter()
        .map(|row| match *row {
            Row::Header {
                workspace, members, ..
            } => format!(
                "H:{}({members})",
                snapshot.workspaces[workspace as usize].label
            ),
            Row::Tab { tab } => format!("T:{}", snapshot.tabs[tab as usize].tab_id),
            Row::Run { len, .. } => format!("R:{len}"),
            Row::Filtered { hidden } => format!("F:{hidden}"),
        })
        .collect()
}

#[test]
fn filters_off_build_the_same_rows_as_no_filter_at_all() {
    let snapshot = fixture();
    let now = Instant::now();
    let unfiltered = SidebarModel::built(ModelInputs::new(&snapshot, &HashSet::new()));
    let off = build(&snapshot, &SidebarFilter::default(), None, now);
    let blank = build(&snapshot, &query("   "), None, now);
    for model in [&off, &blank] {
        assert_eq!(model.rows, unfiltered.rows);
        assert_eq!((model.filtered, model.first_match), (0, None));
        assert_eq!(model.filter_deadline, None);
    }
    assert_eq!(
        rows(&snapshot, &unfiltered),
        [
            "T:tab_1",
            "T:tab_2",
            "H:api(5)",
            "T:tab_3",
            "R:4",
            "H:infra(1)",
            "T:tab_8"
        ]
    );
}

#[test]
fn search_matches_label_group_and_agent_kind_case_insensitively() {
    let snapshot = fixture();
    let now = Instant::now();
    // A label, any case.
    let model = build(&snapshot, &query("REVIEW"), None, now);
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "T:tab_2", "F:6"]);
    assert_eq!(model.first_match, Some(1), "the focused lead is no match");
    // A group's name keeps the group with all its tabs, header included.
    let model = build(&snapshot, &query("infra"), None, now);
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:infra(1)", "T:tab_8", "F:6"]
    );
    // An agent kind.
    let model = build(&snapshot, &query("codex"), None, now);
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "T:tab_2", "F:6"]);
    // No match anywhere: only the focused tab and the escape row.
    let model = build(&snapshot, &query("nothing"), None, now);
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "F:7"]);
    assert_eq!(model.first_match, None);
}

#[test]
fn every_search_term_must_match_somewhere() {
    let snapshot = fixture();
    let now = Instant::now();
    // `api` matches the group, `notes` one label of it.
    let model = build(&snapshot, &query("api notes"), None, now);
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:api(1)", "T:tab_3", "F:6"]
    );
    // `claude` matches every claude tab, `deploy` narrows to one.
    let model = build(&snapshot, &query("claude deploy"), None, now);
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:infra(1)", "T:tab_8", "F:6"]
    );
    let model = build(&snapshot, &query("codex deploy"), None, now);
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "F:7"]);
}

#[test]
fn a_suspended_run_holding_a_match_unfolds_into_its_matching_tabs() {
    let snapshot = fixture();
    let model = build(&snapshot, &query("parked"), None, Instant::now());
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:api(4)", "T:tab_4", "T:tab_5", "T:tab_6", "T:tab_7", "F:3"],
        "no run row while filtering"
    );
    let model = build(&snapshot, &query("parked 6"), None, Instant::now());
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:api(1)", "T:tab_6", "F:6"]
    );
    assert_eq!(model.first_match, Some(5));
}

#[test]
fn a_folded_group_with_a_match_is_listed_open_while_filtering() {
    let snapshot = fixture();
    let collapsed: HashSet<String> = [group_key("ws_3")].into_iter().collect();
    let filter = query("deploy");
    let folded = SidebarModel::built(ModelInputs::new(&snapshot, &collapsed));
    assert!(!rows(&snapshot, &folded).contains(&"T:tab_8".to_owned()));
    let model = SidebarModel::built(ModelInputs {
        filter: Some(&filter),
        ..ModelInputs::new(&snapshot, &collapsed)
    });
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:infra(1)", "T:tab_8", "F:6"]
    );
}

#[test]
fn the_focused_tab_stays_listed_but_never_counts_as_a_match() {
    let mut snapshot = fixture();
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == "tab_6";
    }
    let model = build(&snapshot, &query("deploy"), None, Instant::now());
    assert_eq!(
        rows(&snapshot, &model),
        ["H:api(1)", "T:tab_6", "H:infra(1)", "T:tab_8", "F:6"],
        "the focused parked tab keeps its group"
    );
    assert_eq!(model.first_match, Some(7));
    assert!(model
        .tab(5)
        .is_some_and(|facts| !facts.hidden && !facts.filter_match));
}

#[test]
fn recency_keeps_live_and_recent_tabs_and_counts_unknown_times_as_old() {
    let snapshot = fixture();
    let now = Instant::now();
    // lead works (live, no time needed); reviewer idle 10m; notes idle 40m;
    // parked 4 suspended 5m, the other parked tabs have no time; deploy 3h.
    let times = times(
        now,
        &[
            None,
            Some(10),
            Some(40),
            Some(5),
            None,
            None,
            None,
            Some(180),
        ],
    );
    let mut filter = SidebarFilter::default();
    filter.recency = Recency::M15;
    let model = build(&snapshot, &filter, Some(&times), now);
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "T:tab_2", "H:api(1)", "T:tab_4", "F:5"]
    );
    // The window next drops reviewer (10m ago, 15m window), before parked 4.
    assert_eq!(
        model.filter_deadline,
        Some(now - Duration::from_secs(10 * 60) + Duration::from_secs(15 * 60))
    );
    filter.recency = Recency::H6;
    let model = build(&snapshot, &filter, Some(&times), now);
    assert_eq!(
        rows(&snapshot, &model),
        [
            "T:tab_1",
            "T:tab_2",
            "H:api(2)",
            "T:tab_3",
            "T:tab_4",
            "H:infra(1)",
            "T:tab_8",
            "F:3"
        ]
    );
    // Without any agent times every non-live tab is old.
    let model = build(&snapshot, &filter, None, now);
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "F:7"]);
}

#[test]
fn a_tab_ages_out_when_the_window_passes_without_new_data() {
    let snapshot = fixture();
    let now = Instant::now();
    let times = times(now, &[None, Some(10)]);
    let mut filter = SidebarFilter::default();
    filter.recency = Recency::M15;
    let collapsed = HashSet::new();
    let inputs = |at: Instant| ModelInputs {
        filter: Some(&filter),
        times: Some(&times),
        now: Some(at),
        ..ModelInputs::new(&snapshot, &collapsed)
    };
    let mut model = SidebarModel::new();
    model.ensure(inputs(now));
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "T:tab_2", "F:6"]);
    let builds = model.builds;
    model.ensure(inputs(now + Duration::from_secs(4 * 60)));
    assert_eq!(model.builds, builds, "no rebuild before the deadline");
    model.ensure(inputs(now + Duration::from_secs(5 * 60)));
    assert_eq!(model.builds, builds + 1, "the deadline rebuilds");
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "F:7"]);
    assert_eq!(model.filter_deadline, None, "nothing left to age out");
    model.ensure(inputs(now + Duration::from_secs(60 * 60)));
    assert_eq!(model.builds, builds + 1);
}

#[test]
fn search_and_recency_compose_by_and() {
    let snapshot = fixture();
    let now = Instant::now();
    let times = times(
        now,
        &[None, Some(10), Some(40), None, None, None, None, Some(5)],
    );
    let mut filter = query("claude");
    filter.recency = Recency::M15;
    let model = build(&snapshot, &filter, Some(&times), now);
    // claude: lead, notes, parked, deploy; recent: lead, reviewer, deploy.
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:infra(1)", "T:tab_8", "F:6"]
    );
    assert_eq!(model.first_match, Some(0), "the focused lead matches both");
}

#[test]
fn fixed_tabs_and_the_side_blocks_ignore_the_filters() {
    let mut snapshot = fixture();
    snapshot.tabs[2].agent_status = AgentStatus::Blocked;
    snapshot.agents[2].agent_status = AgentStatus::Blocked;
    let collapsed = HashSet::new();
    let filter = query("deploy");
    let model = SidebarModel::built(ModelInputs {
        filter: Some(&filter),
        fixed_ids: (Some("tab_2"), None),
        ..ModelInputs::new(&snapshot, &collapsed)
    });
    assert_eq!(
        rows(&snapshot, &model),
        ["T:tab_1", "H:infra(1)", "T:tab_8", "F:5"],
        "the fixed tab is neither listed nor counted"
    );
    let active: Vec<u32> = model.active.iter().map(|entry| entry.tab).collect();
    assert!(active.contains(&2), "the blocked notes tab stays Active");
}

// ------------------------------------------------------------ the client

fn filter_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(fixture()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("composed frame");
    state
}

fn listed(state: &ClientShellState) -> Vec<&str> {
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(_, id)| id.as_str())
        .collect()
}

fn row_text(frame: &FrameData, rect: ratatui::layout::Rect) -> String {
    (rect.x..rect.right())
        .map(|x| {
            frame.cells[(rect.y * frame.width + x) as usize]
                .symbol
                .as_str()
        })
        .collect()
}

fn mouse(
    state: &mut ClientShellState,
    kind: MouseEventKind,
    rect: ratatui::layout::Rect,
    modifiers: KeyModifiers,
) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind,
        column: rect.x,
        row: rect.y,
        modifiers,
    })])
}

fn click(state: &mut ClientShellState, rect: ratatui::layout::Rect) -> ClientShellInput {
    mouse(
        state,
        MouseEventKind::Down(MouseButton::Left),
        rect,
        KeyModifiers::empty(),
    )
}

fn key(state: &mut ClientShellState, sequence: &str) -> ClientShellInput {
    let key = crate::input::parse_terminal_key_sequence(sequence).expect("key");
    state.handle_raw_events(vec![RawInputEvent::Key(key)])
}

fn pane_inputs(outcome: &ClientShellInput) -> usize {
    outcome
        .requests
        .iter()
        .filter(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. }))
        .count()
}

fn focused_tabs(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::TabFocus(target) => Some(target.tab_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn the_toolbar_shows_the_controls_only_on_hover_or_while_engaged() {
    let mut state = filter_state();
    let frame = state.compose(106, 30).expect("composed frame");
    let toolbar = state.hits.sidebar_toolbar;
    assert!(!toolbar.is_empty() && !state.hits.sidebar_search.is_empty());
    let idle = row_text(&frame, toolbar);
    assert!(
        !idle.contains("search") && !idle.contains('\u{25F7}'),
        "{idle:?}"
    );

    let search = state.hits.sidebar_search;
    mouse(
        &mut state,
        MouseEventKind::Moved,
        search,
        KeyModifiers::empty(),
    );
    let frame = state.compose(106, 30).expect("composed frame");
    let hovered = row_text(&frame, toolbar);
    assert!(
        hovered.contains("\u{2315} search") && hovered.contains('\u{25F7}'),
        "{hovered:?}"
    );

    // Engaged (recency on): shown without a hover.
    state.sidebar_hover = None;
    state.sidebar_filter.recency = Recency::H1;
    let frame = state.compose(106, 30).expect("composed frame");
    assert!(row_text(&frame, toolbar).contains("\u{25F7} 1h"));
}

#[test]
fn the_search_field_takes_every_key_and_esc_clears_it() {
    let mut state = filter_state();
    // prefix+f focuses the field.
    let outcome = state.handle_input_bytes(b"\x02f");
    assert!(state.sidebar_search_focused());
    assert_eq!(pane_inputs(&outcome), 0);

    let outcome = state.handle_input_bytes(b"Revi");
    assert_eq!(pane_inputs(&outcome), 0, "no key reaches the pane");
    assert_eq!(state.sidebar_filter.query(), "Revi");
    let frame = state.compose(106, 30).expect("composed frame");
    assert_eq!(listed(&state), ["tab_1", "tab_2"]);
    assert!(frame.cursor.is_none(), "the pane's cursor hides");
    assert!(row_text(&frame, state.hits.sidebar_search).contains("Revi"));

    // Backspace, then a committed text (IME / paste path).
    key(&mut state, "\x7f");
    assert_eq!(state.sidebar_filter.query(), "Rev");
    let outcome = state.handle_raw_events(vec![RawInputEvent::Paste("x\ny".into())]);
    assert_eq!(pane_inputs(&outcome), 0);
    assert_eq!(state.sidebar_filter.query(), "Revx y");

    // Esc clears and lets go; keys reach the pane again.
    key(&mut state, "\x1b[27u");
    assert!(!state.sidebar_search_focused());
    assert_eq!(state.sidebar_filter.query(), "");
    state.compose(106, 30).expect("composed frame");
    assert_eq!(listed(&state).len(), 4, "the list is whole again");
    assert!(pane_inputs(&state.handle_input_bytes(b"a")) > 0);
}

#[test]
fn enter_focuses_the_first_match_even_before_a_compose() {
    let mut state = filter_state();
    let rect = state.hits.sidebar_search;
    click(&mut state, rect);
    assert!(state.sidebar_search_focused());
    // Typed and confirmed in one batch: the model catches up first.
    let outcome = state.handle_input_bytes(b"deploy\r");
    assert_eq!(focused_tabs(&outcome), ["tab_8"]);
    assert!(!state.sidebar_search_focused(), "Enter lets go");
    assert_eq!(state.sidebar_filter.query(), "deploy", "the query stays");
}

#[test]
fn a_press_elsewhere_or_the_key_again_lets_go_of_the_keyboard() {
    let mut state = filter_state();
    state.handle_input_bytes(b"\x02f");
    assert!(state.sidebar_search_focused());
    state.handle_input_bytes(b"\x02f");
    assert!(!state.sidebar_search_focused(), "the key toggles");

    let rect = state.hits.sidebar_search;
    click(&mut state, rect);
    assert!(state.sidebar_search_focused(), "a click on the field");
    let pane = state.hits.panes[0].inner_rect;
    click(&mut state, pane);
    assert!(!state.sidebar_search_focused(), "a pane press");
}

#[test]
fn the_recency_chip_steps_forward_on_click_and_back_on_shift_or_right_click() {
    let mut state = filter_state();
    let chip = state.hits.sidebar_recency;
    click(&mut state, chip);
    assert_eq!(state.sidebar_filter.recency, Recency::M15);
    click(&mut state, chip);
    assert_eq!(state.sidebar_filter.recency, Recency::M30);
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        chip,
        KeyModifiers::SHIFT,
    );
    assert_eq!(state.sidebar_filter.recency, Recency::M15);
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        chip,
        KeyModifiers::empty(),
    );
    assert_eq!(state.sidebar_filter.recency, Recency::Off);
    assert!(state.overlay.is_none(), "no menu opens on the chip");
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        chip,
        KeyModifiers::empty(),
    );
    assert_eq!(state.sidebar_filter.recency, Recency::D1, "back wraps");
}

#[test]
fn filters_are_not_kept_across_a_restart() {
    // `ClientChromePreferences` has no filter field: nothing is saved, and
    // a new client starts unfiltered.
    let mut state = filter_state();
    state.sidebar_filter.set_query("deploy");
    state.sidebar_filter.recency = Recency::H1;
    let fresh = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    assert_eq!(fresh.sidebar_filter, SidebarFilter::default());
}

#[test]
fn compose_does_not_rebuild_the_model_while_a_filter_is_set() {
    let mut state = filter_state();
    state.handle_input_bytes(b"\x02fnotes");
    state.compose(106, 30).expect("composed frame");
    let builds = state.sidebar_model.builds;
    for _ in 0..3 {
        state.compose(106, 30).expect("composed frame");
    }
    assert_eq!(state.sidebar_model.builds, builds);
}

#[test]
fn an_agent_time_behind_the_snapshot_counts_as_recent() {
    let mut snapshot = fixture();
    let now = Instant::now();
    // reviewer's push is two hours old but for an older state: it just changed.
    let mut times = times(now, &[None, Some(120), Some(120)]);
    times
        .panes
        .insert("pane_2".into(), (0, now - Duration::from_secs(7200)));
    snapshot.agents[2].state_change_seq = 1;
    let mut filter = SidebarFilter::default();
    filter.recency = Recency::M15;
    let model = build(&snapshot, &filter, Some(&times), now);
    assert_eq!(rows(&snapshot, &model), ["T:tab_1", "T:tab_2", "F:6"]);
}

#[test]
fn a_literal_prefix_never_reaches_the_pane_while_the_field_is_focused() {
    let mut state = filter_state();
    state.handle_input_bytes(b"\x02f");
    assert!(state.sidebar_search_focused());
    let outcome = state.handle_input_bytes(b"\x02\x02");
    assert_eq!(pane_inputs(&outcome), 0);
    assert!(state.sidebar_search_focused());
    state.handle_input_bytes(b"\x02f");
    assert!(!state.sidebar_search_focused(), "prefix+f still toggles");
}

#[test]
fn a_hidden_sidebar_lets_go_of_the_field_for_good() {
    let mut state = filter_state();
    state.handle_input_bytes(b"\x02f");
    state.sidebar_collapsed = true;
    state.compose(106, 30).expect("composed frame");
    state.sidebar_collapsed = false;
    state.compose(106, 30).expect("composed frame");
    assert!(!state.sidebar_search_focused(), "no focus comes back");
    assert!(pane_inputs(&state.handle_input_bytes(b"a")) > 0);
}

#[test]
fn opening_the_info_pane_takes_the_keyboard_from_the_field() {
    let mut state = filter_state();
    state.handle_input_bytes(b"\x02f");
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane("tab_1".into(), &mut outcome);
    assert!(!state.sidebar_filter.focused);
}

#[test]
fn fold_all_waits_while_a_filter_is_set_and_the_chip_keeps_its_hover() {
    let mut state = filter_state();
    let chip = state.hits.sidebar_recency;
    click(&mut state, chip);
    assert_eq!(state.sidebar_hover, Some(SidebarHover::Toolbar));
    let before = state.collapsed_groups.clone();
    let mut outcome = ClientShellInput::default();
    state.toggle_all_groups_folded(&mut outcome);
    assert_eq!(state.collapsed_groups, before);
    state.sidebar_filter.clear();
    state.toggle_all_groups_folded(&mut outcome);
    assert_ne!(
        state.collapsed_groups, before,
        "folds again once unfiltered"
    );
}

mod fork_smoke {
    use super::*;

    #[test]
    fn typing_in_the_search_field_narrows_the_list_and_the_filtered_row_clears_it() {
        let mut state = filter_state();
        assert_eq!(listed(&state), ["tab_1", "tab_2", "tab_3", "tab_8"]);
        let rect = state.hits.sidebar_search;
        click(&mut state, rect);
        let outcome = state.handle_input_bytes(b"api");
        assert_eq!(pane_inputs(&outcome), 0);
        let frame = state.compose(106, 30).expect("composed frame");
        assert_eq!(
            listed(&state),
            ["tab_1", "tab_3", "tab_4", "tab_5", "tab_6", "tab_7"],
            "the api group, its run unfolded"
        );
        let filtered = state.hits.sidebar_filtered;
        assert!(
            row_text(&frame, filtered).contains("~ 2 filtered"),
            "{:?}",
            row_text(&frame, filtered)
        );
        click(&mut state, filtered);
        assert_eq!(state.sidebar_filter, SidebarFilter::default());
        state.compose(106, 30).expect("composed frame");
        assert_eq!(listed(&state), ["tab_1", "tab_2", "tab_3", "tab_8"]);
        assert!(state.hits.sidebar_filtered.is_empty());
    }
}
