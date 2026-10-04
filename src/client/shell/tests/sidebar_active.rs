//! The tabs sidebar's Active agents block, detail strip and derived model
//! (sidebar v2). S0b seeds the model's architecture contract; the block's
//! tests land with it.

use super::*;
use crate::config::{Config, SidebarLayoutConfig};

fn tabs_state() -> ClientShellState {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

#[test]
fn sidebar_model_builds_once_until_the_snapshot_changes() {
    let mut state = tabs_state();
    for _ in 0..3 {
        state.compose(106, 30).expect("composed frame");
    }
    assert_eq!(
        state.sidebar_model.builds, 1,
        "static frames reuse the model"
    );
    state.set_snapshot(Box::new(snapshot()));
    state.compose(106, 30).expect("composed frame");
    assert_eq!(state.sidebar_model.builds, 2, "a new snapshot rebuilds it");
}
