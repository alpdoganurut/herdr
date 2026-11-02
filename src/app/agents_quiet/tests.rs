use super::*;
use crate::app::agents_model::tests::{
    call, code, model_app, pane, public, terminal_mut, user_turn,
};
use crate::detect::{Agent, AgentState};

fn set_quiet(
    app: &mut App,
    caller: Option<&str>,
    target: &str,
    muted: Option<bool>,
    dnd: Option<bool>,
) -> serde_json::Value {
    call(
        app,
        Method::AgentsSetQuiet(AgentsSetQuietParams {
            caller_pane: caller.map(str::to_string),
            target: target.into(),
            muted,
            dnd,
        }),
    )
}

fn tab(app: &App, ws_idx: usize, tab_idx: usize) -> &crate::workspace::Tab {
    &app.state.workspaces[ws_idx].tabs[tab_idx]
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

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-agents-quiet-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn a_member_mutes_and_dnds_its_teammates_and_is_recorded() {
    let mut app = model_app();
    let dir = temp_dir("member");
    app.agents_model.dir = Some(dir.clone());
    let (lead, fixer) = (public(&app, 1, 0), public(&app, 1, 1));
    // By pane id, no user turn needed: a soft edit in the team.
    let out = set_quiet(&mut app, Some(&lead), &fixer, Some(true), None);
    assert_eq!(code(&out), "ok", "{out}");
    assert_eq!(out["result"]["tab"]["muted"], true, "{out}");
    assert!(tab(&app, 1, 1).muted && !tab(&app, 1, 1).dnd);
    // By tab label, both at once.
    let out = set_quiet(&mut app, Some(&lead), "fixer", Some(false), Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(!tab(&app, 1, 1).muted && tab(&app, 1, 1).dnd);
    let out = set_quiet(&mut app, Some(&lead), &fixer, None, Some(false));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(!tab(&app, 1, 1).dnd);
    // Any member, not only a lead: the fixer mutes the lead's tab, and its own.
    let out = set_quiet(&mut app, Some(&fixer), &lead, Some(true), None);
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 1, 0).muted);
    let out = set_quiet(&mut app, Some(&fixer), &fixer, None, Some(true));
    assert_eq!(code(&out), "ok", "{out}");

    let log = crate::agents_model::actions_log::read_tail(&dir, 10);
    let lead_mute = log
        .iter()
        .rev()
        .find(|entry| entry.action == "set_quiet")
        .expect("logged");
    assert_eq!(lead_mute.outcome, AgentsActionOutcome::Ok);
    assert_eq!(lead_mute.actor_pane.as_deref(), Some(lead.as_str()));
    assert_eq!(lead_mute.detail.as_deref(), Some("muted"));
    assert_eq!(
        lead_mute.target_tab.as_deref(),
        app.public_tab_id(1, 1).as_deref()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn outsiders_and_the_coordinators_tab_are_refused() {
    let mut app = model_app();
    let dir = temp_dir("outsider");
    app.agents_model.dir = Some(dir.clone());
    let (member, fixer, outsider) = (public(&app, 1, 0), public(&app, 1, 1), public(&app, 2, 0));
    let out = set_quiet(&mut app, Some(&outsider), &fixer, Some(true), None);
    assert_eq!(code(&out), "outside_team", "{out}");
    let out = set_quiet(&mut app, Some(&outsider), &fixer, None, Some(true));
    assert_eq!(code(&out), "outside_team", "{out}");
    assert!(!tab(&app, 1, 1).muted && !tab(&app, 1, 1).dnd);
    // Without a team: its own tab only.
    let own = public(&app, 2, 0);
    let out = set_quiet(&mut app, Some(&outsider), &own, Some(true), None);
    assert_eq!(code(&out), "ok", "{out}");
    let shell = app.public_tab_id(2, 1).unwrap();
    let out = set_quiet(&mut app, Some(&outsider), &shell, Some(true), None);
    assert_eq!(code(&out), "outside_team", "{out}");

    let coordinator = with_coordinator(&mut app);
    let out = set_quiet(&mut app, Some(&member), &coordinator, Some(true), None);
    assert_eq!(code(&out), "protected_tab", "{out}");
    assert!(!tab(&app, 0, 0).muted);
    let log = crate::agents_model::actions_log::read_tail(&dir, 1);
    assert_eq!(log[0].outcome, AgentsActionOutcome::Denied);
    assert_eq!(log[0].action, "set_quiet");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_coordinator_needs_its_users_turn_and_the_user_needs_nothing() {
    let mut app = model_app();
    let fixer = public(&app, 1, 1);
    let coordinator = with_coordinator(&mut app);
    let out = set_quiet(&mut app, Some(&coordinator), &fixer, Some(true), None);
    assert_eq!(code(&out), "non_user_turn", "{out}");
    let coordinator_pane = pane(&app, 0, 0);
    user_turn(&mut app, coordinator_pane);
    let out = set_quiet(&mut app, Some(&coordinator), &fixer, Some(true), Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 1, 1).muted && tab(&app, 1, 1).dnd);
    // Not even in its user's turn: its own tab is the user's.
    let own_tab = app.public_tab_id(0, 0).unwrap();
    for target in [coordinator.as_str(), own_tab.as_str()] {
        let out = set_quiet(&mut app, Some(&coordinator), target, Some(true), None);
        assert_eq!(code(&out), "protected_tab", "{out}");
    }
    assert!(!tab(&app, 0, 0).muted);

    // The user (no caller): any tab, by agent pane or tab id.
    let plain_tab = app.public_tab_id(2, 1).unwrap();
    let out = set_quiet(&mut app, None, &plain_tab, Some(true), None);
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 2, 1).muted);
    let out = set_quiet(&mut app, None, &coordinator, None, Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 0, 0).dnd);
}

#[test]
fn an_agents_or_the_coordinators_dnd_is_theirs_and_the_users_is_the_users() {
    let mut app = model_app();
    let (lead, fixer) = (public(&app, 1, 0), public(&app, 1, 1));
    let out = set_quiet(&mut app, Some(&lead), &fixer, None, Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 1, 1).dnd && tab(&app, 1, 1).dnd_by_agent);
    assert!(
        out["result"]["tab"].get("dnd_by_agent").is_none(),
        "not in TabInfo"
    );
    // Off clears who turned it on.
    let out = set_quiet(&mut app, Some(&lead), &fixer, None, Some(false));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(!tab(&app, 1, 1).dnd && !tab(&app, 1, 1).dnd_by_agent);

    let coordinator = with_coordinator(&mut app);
    let coordinator_pane = pane(&app, 0, 0);
    user_turn(&mut app, coordinator_pane);
    let out = set_quiet(&mut app, Some(&coordinator), &fixer, None, Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(
        tab(&app, 1, 1).dnd_by_agent,
        "the coordinator's counts as an agent's"
    );

    // The user (no caller, the CLI as the user) takes it over.
    let out = set_quiet(&mut app, None, &fixer, None, Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 1, 1).dnd && !tab(&app, 1, 1).dnd_by_agent);
    let out = set_quiet(&mut app, None, &lead, None, Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 1, 0).dnd && !tab(&app, 1, 0).dnd_by_agent);
}

#[test]
fn a_shell_pane_caller_is_the_user_for_dnd() {
    let mut app = model_app();
    let fixer = public(&app, 1, 1);
    // The plain group's second tab is a shell: its caller acts as the user.
    let shell = public(&app, 2, 1);
    let out = set_quiet(&mut app, Some(&shell), &fixer, None, Some(true));
    assert_eq!(code(&out), "ok", "{out}");
    assert!(tab(&app, 1, 1).dnd && !tab(&app, 1, 1).dnd_by_agent);
}

#[test]
fn a_request_without_a_change_or_target_is_invalid() {
    let mut app = model_app();
    let lead = public(&app, 1, 0);
    let fixer = public(&app, 1, 1);
    let out = set_quiet(&mut app, Some(&lead), &fixer, None, None);
    assert_eq!(code(&out), "invalid_params", "{out}");
    let out = set_quiet(&mut app, Some(&lead), "nobody-here", Some(true), None);
    assert_eq!(code(&out), "not_found", "{out}");
}

#[test]
fn quiet_details_name_what_changed() {
    assert_eq!(quiet_detail(Some(true), None), "muted");
    assert_eq!(quiet_detail(Some(false), Some(true)), "unmuted, dnd on");
    assert_eq!(quiet_detail(None, Some(false)), "dnd off");
}
