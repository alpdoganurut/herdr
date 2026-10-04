use super::*;
use crate::app::agents_model::tests::{
    call, code, model_app, pane, public, terminal_mut, user_turn,
};
use crate::detect::{Agent, AgentState};

fn group_labels(app: &App) -> Vec<String> {
    (0..app.state.workspaces.len())
        .map(|ws_idx| app.group_label(ws_idx))
        .collect()
}

fn tab_labels(app: &App, ws_idx: usize) -> Vec<String> {
    (0..app.state.workspaces[ws_idx].tabs.len())
        .map(|tab_idx| app.tab_label(ws_idx, tab_idx))
        .collect()
}

fn reorder_tab(app: &mut App, caller: &str, target: &str, position: u32) -> serde_json::Value {
    call(
        app,
        Method::AgentsReorderTab(AgentsReorderTabParams {
            caller_pane: caller.into(),
            target: target.into(),
            position,
        }),
    )
}

fn reorder_group(
    app: &mut App,
    caller: &str,
    params: AgentsReorderGroupParams,
) -> serde_json::Value {
    call(
        app,
        Method::AgentsReorderGroup(AgentsReorderGroupParams {
            caller_pane: caller.into(),
            ..params
        }),
    )
}

/// The bucket's first tab hosts the coordinator.
fn with_coordinator(app: &mut App) -> String {
    let coordinator = pane(app, 0, 0);
    terminal_mut(app, coordinator).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.state.coordinator_terminal_id = Some(
        app.state.workspaces[0]
            .pane_state(coordinator)
            .unwrap()
            .attached_terminal_id
            .clone(),
    );
    public(app, 0, 0)
}

#[test]
fn the_insert_index_lands_the_item_at_its_target() {
    for len in 1..6usize {
        for source in 0..len {
            for target in 0..len {
                let mut items: Vec<usize> = (0..len).collect();
                let insert = insert_index_for(source, target);
                let item = items.remove(source);
                let at = if source < insert { insert - 1 } else { insert };
                items.insert(at, item);
                assert_eq!(items[target], source, "{source} -> {target} of {len}");
            }
        }
    }
}

#[test]
fn a_member_reorders_its_teams_tabs_but_nothing_outside_it() {
    let mut app = model_app();
    let (member, plain_agent) = (public(&app, 1, 0), public(&app, 2, 0));
    let before = tab_labels(&app, 1);
    let fixer = public(&app, 1, 1);
    let out = reorder_tab(&mut app, &member, &fixer, 1);
    assert_eq!(code(&out), "ok", "{out}");
    assert_eq!(out["result"]["reorder"]["position"], 1);
    assert_eq!(out["result"]["reorder"]["moved"], true);
    assert_eq!(before[1], "fixer");
    assert_eq!(
        tab_labels(&app, 1)[0],
        "fixer",
        "unnamed tabs show their number"
    );
    // Already there: nothing moves.
    let first = public(&app, 1, 0);
    let again = reorder_tab(&mut app, &member, &first, 1);
    assert_eq!(again["result"]["reorder"]["moved"], false, "{again}");

    // Another group's tabs: read and message only.
    let plain_tab = public(&app, 2, 1);
    let out = reorder_tab(&mut app, &member, &plain_tab, 1);
    assert_eq!(code(&out), "outside_team", "{out}");
    // Outside a team even its own tab: it moves the group's other tabs.
    let own = public(&app, 2, 0);
    let out = reorder_tab(&mut app, &plain_agent, &own, 2);
    assert_eq!(code(&out), "outside_team", "{out}");
}

#[test]
fn only_the_coordinator_in_its_users_turn_reorders_groups() {
    let mut app = model_app();
    let member = public(&app, 1, 0);
    let plain = AgentsReorderGroupParams {
        group: "plain".into(),
        position: Some(1),
        ..AgentsReorderGroupParams::default()
    };
    let out = reorder_group(&mut app, &member, plain.clone());
    assert_eq!(code(&out), "coordinator_only", "{out}");
    let coordinator = with_coordinator(&mut app);
    let out = reorder_group(&mut app, &coordinator, plain.clone());
    assert_eq!(code(&out), "non_user_turn", "{out}");
    let start = group_labels(&app);
    assert_eq!(start[1..], ["team", "plain"]);

    let coordinator_pane = pane(&app, 0, 0);
    user_turn(&mut app, coordinator_pane);
    let out = reorder_group(&mut app, &coordinator, plain);
    assert_eq!(code(&out), "ok", "{out}");
    assert_eq!(group_labels(&app)[1..], ["plain", "team"]);
    assert_eq!(out["result"]["reorder"]["position"], 1);
    assert_eq!(out["result"]["reorder"]["of"], 2);

    // after / before another group.
    let out = reorder_group(
        &mut app,
        &coordinator,
        AgentsReorderGroupParams {
            group: "plain".into(),
            after: Some("team".into()),
            ..AgentsReorderGroupParams::default()
        },
    );
    assert_eq!(code(&out), "ok", "{out}");
    assert_eq!(group_labels(&app)[1..], ["team", "plain"]);
    let out = reorder_group(
        &mut app,
        &coordinator,
        AgentsReorderGroupParams {
            group: "plain".into(),
            before: Some("team".into()),
            ..AgentsReorderGroupParams::default()
        },
    );
    assert_eq!(code(&out), "ok", "{out}");
    assert_eq!(group_labels(&app)[1..], ["plain", "team"]);

    // The first space stays first; one placement only.
    let first = group_labels(&app)[0].clone();
    let out = reorder_group(
        &mut app,
        &coordinator,
        AgentsReorderGroupParams {
            group: first,
            position: Some(2),
            ..AgentsReorderGroupParams::default()
        },
    );
    assert_eq!(code(&out), "invalid_params", "{out}");
    let out = reorder_group(
        &mut app,
        &coordinator,
        AgentsReorderGroupParams {
            group: "team".into(),
            position: Some(1),
            before: Some("plain".into()),
            ..AgentsReorderGroupParams::default()
        },
    );
    assert_eq!(code(&out), "invalid_params", "{out}");
}

#[test]
fn the_coordinators_own_tab_is_not_reordered() {
    let mut app = model_app();
    let coordinator = with_coordinator(&mut app);
    let coordinator_pane = pane(&app, 0, 0);
    user_turn(&mut app, coordinator_pane);
    let out = reorder_tab(&mut app, &coordinator, &coordinator, 1);
    assert_eq!(code(&out), "protected_tab", "{out}");
    // Any other tab, in the user's turn.
    let fixer = public(&app, 1, 1);
    let out = reorder_tab(&mut app, &coordinator, &fixer, 1);
    assert_eq!(code(&out), "ok", "{out}");
}
