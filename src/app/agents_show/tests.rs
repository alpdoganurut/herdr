use super::*;
use crate::app::agents_model::tests::{
    call, code, model_app, pane, public, terminal_mut, user_turn,
};
use crate::detect::{Agent, AgentState};

fn show(app: &mut App, caller: Option<&str>, target: &str) -> serde_json::Value {
    call(
        app,
        Method::AgentsShow(AgentsShowParams {
            caller_pane: caller.map(str::to_string),
            target: target.into(),
        }),
    )
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-agents-show-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
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
fn an_agents_own_turn_is_refused_and_logged() {
    let mut app = model_app();
    let dir = temp_dir("refused");
    app.agents_model.dir = Some(dir.clone());
    let (lead, fixer) = (public(&app, 1, 0), public(&app, 1, 1));
    let active = (app.state.active, app.state.workspaces[1].active_tab_index());
    let out = show(&mut app, Some(&lead), &fixer);
    assert_eq!(code(&out), "non_user_turn", "{out}");
    let hint = out["error"]["message"].as_str().unwrap_or("");
    assert!(hint.contains("not your user"), "{hint}");
    assert_eq!(
        (app.state.active, app.state.workspaces[1].active_tab_index()),
        active,
        "nothing moved"
    );
    let log = crate::agents_model::actions_log::read_tail(&dir, 1);
    assert_eq!(log[0].action, "show");
    assert_eq!(log[0].outcome, AgentsActionOutcome::Denied);
    assert_eq!(log[0].code.as_deref(), Some("non_user_turn"));
    assert_eq!(log[0].actor_pane.as_deref(), Some(lead.as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_users_turn_passes_the_check_for_any_tab_and_needs_a_client() {
    let mut app = model_app();
    let dir = temp_dir("allowed");
    app.agents_model.dir = Some(dir.clone());
    let lead = public(&app, 1, 0);
    let lead_pane = pane(&app, 1, 0);
    user_turn(&mut app, lead_pane);
    // A plain group's agent by pane id and its tab by label; the
    // coordinator's tab too: looking is free everywhere.
    let coordinator = with_coordinator(&mut app);
    for target in [public(&app, 2, 0), "notes".to_string(), coordinator] {
        let out = show(&mut app, Some(&lead), &target);
        assert_eq!(code(&out), "no_client", "{target}: {out}");
    }
    // Without the server no client is attached: refused, logged as failed.
    let log = crate::agents_model::actions_log::read_tail(&dir, 1);
    assert_eq!(log[0].action, "show");
    assert_eq!(log[0].outcome, AgentsActionOutcome::Failed);
    assert_eq!(log[0].code.as_deref(), Some("no_client"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_target_is_not_found() {
    let mut app = model_app();
    let lead = public(&app, 1, 0);
    let lead_pane = pane(&app, 1, 0);
    user_turn(&mut app, lead_pane);
    let out = show(&mut app, Some(&lead), "ghost");
    assert_eq!(code(&out), "not_found", "{out}");
    let out = show(&mut app, Some(&lead), " ");
    assert_eq!(code(&out), "invalid_params", "{out}");
}

#[test]
fn the_coordinator_shows_only_in_its_users_turn() {
    let mut app = model_app();
    let fixer = public(&app, 1, 1);
    let coordinator = with_coordinator(&mut app);
    let out = show(&mut app, Some(&coordinator), &fixer);
    assert_eq!(code(&out), "non_user_turn", "{out}");
    let coordinator_pane = pane(&app, 0, 0);
    user_turn(&mut app, coordinator_pane);
    let out = show(&mut app, Some(&coordinator), &fixer);
    assert_eq!(code(&out), "no_client", "{out}");
}
