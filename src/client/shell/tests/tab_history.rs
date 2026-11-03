//! Tab history (fork, sidebar v3): `cmd+[` / `cmd+]` walk back and forward
//! through the tabs this client focused, browser-style (`tab_history.rs`).

use super::teams::{endpoint_requests, tabs_config, team_snapshot};
use super::*;
use crate::api::schema::Method;
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crate::client::shell::tab_history::{TabHistory, CAP};

fn live(_: &str) -> bool {
    true
}

fn visited(tabs: &[&str]) -> TabHistory {
    let mut history = TabHistory::default();
    for tab in tabs {
        history.observe(tab, live);
    }
    history
}

#[test]
fn history_goes_back_and_forward_like_a_browser() {
    let mut history = visited(&["a", "b", "c"]);
    assert_eq!(history.back(live).as_deref(), Some("b"));
    history.observe("b", live);
    assert_eq!(history.back(live).as_deref(), Some("a"));
    history.observe("a", live);
    assert_eq!(history.back(live), None, "nothing before the first tab");
    assert_eq!(history.forward(live).as_deref(), Some("b"));
    history.observe("b", live);
    assert_eq!(history.forward(live).as_deref(), Some("c"));
    history.observe("c", live);
    assert_eq!(history.forward(live), None, "nothing after the last tab");
    assert_eq!(history.entries(), ["a", "b", "c"]);
}

#[test]
fn a_jump_does_not_push_history() {
    let mut history = visited(&["a", "b", "c"]);
    assert_eq!(history.back(live).as_deref(), Some("b"));
    history.observe("b", live);
    assert_eq!(
        history.entries(),
        ["a", "b", "c"],
        "the landing is no visit"
    );
    assert_eq!(history.focused(), Some("b"));
}

#[test]
fn a_new_visit_after_back_clears_forward() {
    let mut history = visited(&["a", "b", "c"]);
    history.back(live);
    history.observe("b", live);
    history.observe("d", live);
    assert_eq!(history.entries(), ["a", "b", "d"]);
    assert_eq!(history.forward(live), None);
    assert_eq!(history.back(live).as_deref(), Some("b"));
}

#[test]
fn closed_tabs_are_skipped() {
    let mut history = visited(&["a", "b", "c"]);
    assert_eq!(history.back(|tab| tab != "b").as_deref(), Some("a"));
    history.observe("a", live);
    assert_eq!(history.forward(|tab| tab != "b").as_deref(), Some("c"));
    // No live tab at all: nothing to focus.
    let mut history = visited(&["a", "b", "c"]);
    assert_eq!(history.back(|tab| tab == "c"), None);
}

#[test]
fn the_focused_tab_is_never_a_target() {
    // a, b, a with b closed: going back from a would focus a again, a no-op
    // `tab.focus` whose landing snapshot could never confirm the jump.
    let mut history = visited(&["a", "b", "a"]);
    assert_eq!(history.back(|tab| tab != "b"), None);
    history.observe("c", live);
    // The press pruned the closed b and collapsed the two a's.
    assert_eq!(history.entries(), ["a", "c"]);
}

#[test]
fn repeated_ids_do_not_confirm_a_jump_early() {
    let mut history = visited(&["a", "b", "a", "c"]);
    assert_eq!(history.back(live).as_deref(), Some("a"));
    assert_eq!(history.back(live).as_deref(), Some("b"));
    assert_eq!(history.back(live).as_deref(), Some("a"));
    // The three landings arrive in order; the first a is index 2, not 0.
    history.observe("a", live);
    history.observe("b", live);
    history.observe("a", live);
    assert_eq!(
        history.entries(),
        ["a", "b", "a", "c"],
        "no landing is a visit"
    );
    assert_eq!(history.forward(live).as_deref(), Some("b"));
}

#[test]
fn closed_tabs_are_pruned_so_older_live_tabs_stay_reachable() {
    // A full history of tabs opened and closed after "home": the visit that
    // overflows the cap drops the dead ids, not "home".
    let is_live = |tab: &str| tab == "home" || tab == "now";
    let mut history = visited(&["home"]);
    let dead: Vec<String> = (1..CAP).map(|index| format!("gone{index}")).collect();
    for id in &dead {
        history.observe(id, live);
    }
    assert_eq!(history.entries().len(), CAP);
    history.observe("now", is_live);
    assert_eq!(history.entries(), ["home", "now"]);
    assert_eq!(history.back(is_live).as_deref(), Some("home"));
}

#[test]
fn consecutive_duplicates_collapse_and_stacks_cap_at_50() {
    let mut history = visited(&["a", "a", "b", "b", "b", "a"]);
    assert_eq!(history.entries(), ["a", "b", "a"]);

    let ids: Vec<String> = (0..CAP + 10).map(|index| format!("t{index}")).collect();
    for id in &ids {
        history.observe(id, live);
    }
    assert_eq!(history.entries().len(), CAP);
    assert_eq!(history.entries()[0], ids[10]);
    assert_eq!(history.focused(), Some(ids[CAP + 9].as_str()));
    assert_eq!(history.back(live).as_deref(), Some(ids[CAP + 8].as_str()));
}

#[test]
fn a_snapshot_of_the_same_tab_does_not_cancel_a_jump() {
    let mut history = visited(&["a", "b", "c"]);
    assert_eq!(history.back(live).as_deref(), Some("b"));
    // An agent status flip arrives before the focus lands.
    history.observe("c", live);
    history.observe("b", live);
    assert_eq!(history.entries(), ["a", "b", "c"]);
    assert_eq!(history.forward(live).as_deref(), Some("c"));
}

#[test]
fn two_backs_before_the_snapshot_lands_walk_two_steps() {
    let mut history = visited(&["a", "b", "c"]);
    assert_eq!(history.back(live).as_deref(), Some("b"));
    assert_eq!(history.back(live).as_deref(), Some("a"));
    // Both requests land in order; neither is a visit.
    history.observe("b", live);
    history.observe("a", live);
    assert_eq!(history.entries(), ["a", "b", "c"]);
    assert_eq!(history.focused(), Some("a"));
    assert_eq!(history.forward(live).as_deref(), Some("b"));
}

#[test]
fn a_jump_that_never_lands_leaves_history_intact() {
    let mut history = visited(&["a", "b", "c"]);
    assert_eq!(history.back(live).as_deref(), Some("b"));
    // The focus request fails; the user clicks another tab.
    history.observe("d", live);
    assert_eq!(history.entries(), ["a", "b", "c", "d"]);
    assert_eq!(history.back(live).as_deref(), Some("c"));
}

fn focus(snapshot: &mut ClientShellSnapshot, tab_id: &str) {
    snapshot.revision += 1;
    snapshot.focused_tab_id = Some(tab_id.into());
    for tab in &mut snapshot.tabs {
        tab.focused = tab.tab_id == tab_id;
    }
    if let Some(pane) = snapshot.panes.iter().find(|pane| pane.tab_id == tab_id) {
        snapshot.focused_pane_id = Some(pane.pane_id.clone());
        snapshot.focused_workspace_id = Some(pane.workspace_id.clone());
    }
}

/// `tab_1` (first snapshot), then `tab_2`, then `tab_3`.
fn visited_state() -> (ClientShellState, ClientShellSnapshot) {
    visited_state_with(tabs_config())
}

fn visited_state_with(config: crate::config::Config) -> (ClientShellState, ClientShellSnapshot) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let mut snapshot = team_snapshot();
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    for tab in ["tab_2", "tab_3"] {
        focus(&mut snapshot, tab);
        state.set_snapshot(Box::new(snapshot.clone()));
    }
    (state, snapshot)
}

fn press(state: &mut ClientShellState, sequence: &str) -> Vec<String> {
    let key = crate::input::parse_terminal_key_sequence(sequence).expect("kitty key");
    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(key)]);
    endpoint_requests(&outcome)
        .into_iter()
        .filter_map(|(_, method)| match method {
            Method::TabFocus(target) => Some(target.tab_id),
            _ => None,
        })
        .collect()
}

#[test]
fn boot_change_resets_history() {
    let (mut state, mut snapshot) = visited_state();
    assert_eq!(state.tab_history.entries(), ["tab_1", "tab_2", "tab_3"]);
    snapshot.boot_id = "boot-2".into();
    snapshot.revision = 1;
    state.set_snapshot(Box::new(snapshot));
    assert_eq!(state.tab_history.entries(), ["tab_3"]);
    assert!(press(&mut state, "\x1b[91;9u").is_empty());
}

#[test]
fn history_resets_when_the_active_endpoint_changes() {
    let (mut state, _) = visited_state();
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").expect("profile id"),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    let mut remote_snapshot = team_snapshot();
    remote_snapshot.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote, Box::new(remote_snapshot));
    assert!(state.activate_endpoint_projection(&remote));
    assert_eq!(state.tab_history.entries(), ["tab_1"]);
    assert!(press(&mut state, "\x1b[91;9u").is_empty());
}

#[test]
fn availability_follows_the_scan_without_moving_anything() {
    let mut history = visited(&["a", "b", "c"]);
    assert!(history.can_go_back(live));
    assert!(!history.can_go_forward(live));
    assert!(!TabHistory::default().can_go_back(live));
    assert!(!TabHistory::default().can_go_forward(live));
    assert_eq!(history.entries(), ["a", "b", "c"], "nothing pruned");

    // A press in flight counts from its target, like the next press: the
    // only entry after it is the focused tab.
    assert_eq!(history.back(live).as_deref(), Some("b"));
    assert!(history.can_go_back(live));
    assert!(!history.can_go_forward(live));
    history.observe("b", live);
    assert!(history.can_go_forward(live), "landed");
    assert_eq!(history.back(live).as_deref(), Some("a"));
    history.observe("a", live);
    assert!(!history.can_go_back(live));
    assert!(history.back(live).is_none(), "agrees with the press");

    // Closed tabs and the focused tab's other entries do not count.
    let history = visited(&["a", "b", "a", "c"]);
    let only_c = |id: &str| id == "c";
    assert!(!history.can_go_back(only_c));
    let only_ac = |id: &str| id != "b";
    assert!(history.can_go_back(only_ac));
    let history = visited(&["c", "a", "c"]);
    assert!(!history.can_go_back(only_c), "only the focused tab's id");
}

// ------------------------------------------------------- toolbar buttons

/// A frame for the current snapshot (a pane surface of its revision first).
fn composed(state: &mut ClientShellState) -> crate::client::frame_output::ComposedFrame {
    let mut pane = surface();
    if let Some(snapshot) = state.snapshot.as_deref() {
        pane.projection_revision = snapshot.revision;
    }
    state.set_pane_surface(pane);
    state.compose(106, 30).expect("composed frame")
}

fn click(state: &mut ClientShellState, rect: ratatui::layout::Rect) -> ClientShellInput {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn focused_by(outcome: &ClientShellInput) -> Vec<String> {
    endpoint_requests(outcome)
        .into_iter()
        .filter_map(|(_, method)| match method {
            Method::TabFocus(target) => Some(target.tab_id),
            _ => None,
        })
        .collect()
}

/// The toolbar buttons' (`‹`, `›`) foregrounds as palette tokens: `true`
/// when accent, `false` when overlay0.
fn lit(state: &mut ClientShellState) -> (bool, bool) {
    let frame = composed(state);
    let palette = state.config.palette.clone();
    let fg = |rect: ratatui::layout::Rect| {
        let cell = &frame.cells[(rect.y * frame.width + rect.x) as usize];
        if cell.fg == crate::protocol::color_to_u32(palette.accent) {
            true
        } else {
            assert_eq!(cell.fg, crate::protocol::color_to_u32(palette.overlay0));
            false
        }
    };
    (
        fg(state.hits.sidebar_history_back),
        fg(state.hits.sidebar_history_forward),
    )
}

#[test]
fn the_history_buttons_sit_after_the_toggle_slot_before_the_search_field() {
    let (mut state, _) = visited_state();
    let frame = composed(&mut state);
    let (back, forward) = (
        state.hits.sidebar_history_back,
        state.hits.sidebar_history_forward,
    );
    let glyph = |rect: ratatui::layout::Rect| {
        frame.cells[(rect.y * frame.width + rect.x) as usize]
            .symbol
            .clone()
    };
    assert_eq!((back.width, back.height, forward.width), (1, 1, 1));
    assert_eq!(back.y, state.hits.group_new.y, "in the toolbar row");
    assert_eq!(back.x, 3, "after the fold toggle's slot");
    assert_eq!(back.x + 2, forward.x, "one blank cell between");
    assert_eq!(
        forward.x + 2,
        state.hits.sidebar_search.x,
        "one blank cell before the field"
    );
    assert_eq!(
        glyph(back),
        crate::client::shell::sidebar_filter::HISTORY_BACK_GLYPH
    );
    assert_eq!(
        glyph(forward),
        crate::client::shell::sidebar_filter::HISTORY_FORWARD_GLYPH
    );
    assert!(state.sidebar_hover.is_none(), "shown without the pointer");
    for other in [
        state.hits.group_toggle_all,
        state.hits.sidebar_search,
        state.hits.sidebar_recency,
        state.hits.group_new,
    ] {
        assert!(!back.intersects(other) && !forward.intersects(other));
    }
}

#[test]
fn the_history_buttons_are_lit_only_with_history_that_way() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    assert_eq!(lit(&mut state), (false, false), "one entry");

    let (mut state, mut snapshot) = visited_state();
    assert_eq!(lit(&mut state), (true, false));
    assert_eq!(press(&mut state, "\x1b[91;9u"), ["tab_2"]);
    assert_eq!(
        lit(&mut state),
        (true, false),
        "a press in flight counts from its target, like the keys"
    );
    focus(&mut snapshot, "tab_2");
    state.set_snapshot(Box::new(snapshot.clone()));
    assert_eq!(lit(&mut state), (true, true));

    // Every older tab closed: back goes dim.
    snapshot.revision += 1;
    snapshot.tabs.retain(|tab| tab.tab_id != "tab_1");
    snapshot.panes.retain(|pane| pane.tab_id != "tab_1");
    snapshot.agents.retain(|agent| agent.tab_id != "tab_1");
    state.set_snapshot(Box::new(snapshot));
    assert_eq!(lit(&mut state), (false, true));
}

#[test]
fn clicking_the_history_buttons_does_what_the_keys_do() {
    let (mut state, mut snapshot) = visited_state();
    composed(&mut state);
    let (back, forward) = (
        state.hits.sidebar_history_back,
        state.hits.sidebar_history_forward,
    );
    assert_eq!(focused_by(&click(&mut state, back)), ["tab_2"]);
    focus(&mut snapshot, "tab_2");
    state.set_snapshot(Box::new(snapshot.clone()));
    assert_eq!(
        state.tab_history.entries(),
        ["tab_1", "tab_2", "tab_3"],
        "the landing pushes nothing"
    );
    composed(&mut state);
    assert_eq!(focused_by(&click(&mut state, forward)), ["tab_3"]);
    focus(&mut snapshot, "tab_3");
    state.set_snapshot(Box::new(snapshot));
    composed(&mut state);

    // `›` is dim now: a click sends nothing and does not focus the field.
    let outcome = click(&mut state, forward);
    assert!(focused_by(&outcome).is_empty());
    assert!(endpoint_requests(&outcome).is_empty());
    assert!(!state.sidebar_filter.focused);
    assert_eq!(state.tab_history.focused(), Some("tab_3"));
}

#[test]
fn a_click_on_a_dim_history_button_does_nothing() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(team_snapshot()));
    state.set_pane_surface(surface());
    composed(&mut state);
    for rect in [
        state.hits.sidebar_history_back,
        state.hits.sidebar_history_forward,
    ] {
        assert!(!rect.is_empty());
        let outcome = click(&mut state, rect);
        assert!(endpoint_requests(&outcome).is_empty());
        assert!(!state.sidebar_filter.focused, "not the search field's");
    }
    assert_eq!(state.tab_history.entries(), ["tab_1"]);
}

#[test]
fn narrow_toolbars_drop_the_history_buttons_before_the_search_field() {
    let mut config = tabs_config();
    // Fork (session header): 19 content cells, one fewer than the buttons
    // need since the notes button left the toolbar.
    config.ui.sidebar_width = 20;
    let (mut state, _) = visited_state_with(config);
    let frame = composed(&mut state);
    assert!(state.hits.sidebar_history_back.is_empty());
    assert!(state.hits.sidebar_history_forward.is_empty());
    let field = state.hits.sidebar_search;
    assert_eq!(field.x, 3, "the field takes the buttons' cells");
    let row: String = (0..21)
        .map(|x| {
            frame.cells[(field.y * frame.width + x) as usize]
                .symbol
                .as_str()
        })
        .collect();
    assert!(
        !row.contains(crate::client::shell::sidebar_filter::HISTORY_BACK_GLYPH),
        "{row:?}"
    );

    // Without mouse capture the buttons still show, with no hit (the
    // sidebar starts at x=0 and its toolbar at y=0: cell 3 is `‹`).
    let (mut state, _) = visited_state();
    state.config.mouse_capture = false;
    let frame = composed(&mut state);
    assert!(state.hits.sidebar_history_back.is_empty());
    assert_eq!(
        frame.cells[3].symbol,
        crate::client::shell::sidebar_filter::HISTORY_BACK_GLYPH
    );
}

mod fork_smoke {
    use super::*;

    #[test]
    fn cmd_bracket_focuses_the_previous_tab_and_back_again() {
        let (mut state, mut snapshot) = visited_state();

        // cmd+[ (kitty CSI-u, super) asks for the previous tab.
        assert_eq!(press(&mut state, "\x1b[91;9u"), ["tab_2"]);
        focus(&mut snapshot, "tab_2");
        state.set_snapshot(Box::new(snapshot.clone()));
        assert_eq!(
            state.tab_history.entries(),
            ["tab_1", "tab_2", "tab_3"],
            "the landing pushes nothing"
        );

        assert_eq!(press(&mut state, "\x1b[91;9u"), ["tab_1"]);
        focus(&mut snapshot, "tab_1");
        state.set_snapshot(Box::new(snapshot.clone()));
        assert!(press(&mut state, "\x1b[91;9u").is_empty(), "nothing older");

        // cmd+] walks forward again.
        assert_eq!(press(&mut state, "\x1b[93;9u"), ["tab_2"]);
        focus(&mut snapshot, "tab_2");
        state.set_snapshot(Box::new(snapshot.clone()));
        assert_eq!(press(&mut state, "\x1b[93;9u"), ["tab_3"]);
        focus(&mut snapshot, "tab_3");
        state.set_snapshot(Box::new(snapshot.clone()));
        assert!(press(&mut state, "\x1b[93;9u").is_empty(), "nothing newer");

        // A closed tab is skipped.
        snapshot.revision += 1;
        snapshot.tabs.retain(|tab| tab.tab_id != "tab_2");
        snapshot.panes.retain(|pane| pane.tab_id != "tab_2");
        snapshot.agents.retain(|agent| agent.tab_id != "tab_2");
        state.set_snapshot(Box::new(snapshot));
        assert_eq!(press(&mut state, "\x1b[91;9u"), ["tab_1"]);
    }
}
