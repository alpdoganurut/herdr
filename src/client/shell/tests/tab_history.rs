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
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
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
