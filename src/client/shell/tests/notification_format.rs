//! Agent notification text in the `tabs` sidebar layout
//! (`notification_format.rs`): "<tab> finished", body "<agent> · <directory>".

use super::*;
use crate::config::{Config, SidebarLayoutConfig};

fn agent(pane_id: &str, tab_id: &str, status: AgentStatus) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: tab_id.into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 2,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }
}

/// tab_1 focused; tab_2 "level plan" in the background, its pane_2 in
/// /home/me/leap-bi-4 hosting a blocked claude.
fn background_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.workspaces[0].label = "leap-bi-4".into();
    let mut tab = snapshot.tabs[0].clone();
    tab.tab_id = "tab_2".into();
    tab.label = "level plan".into();
    tab.focused = false;
    tab.agent_status = AgentStatus::Blocked;
    snapshot.tabs.push(tab);
    let mut pane = snapshot.panes[0].clone();
    pane.pane_id = "pane_2".into();
    pane.tab_id = "tab_2".into();
    pane.cwd = Some("/home/me/leap-bi-4".into());
    pane.focused = false;
    snapshot.panes.push(pane);
    snapshot.agents = vec![agent("pane_2", "tab_2", AgentStatus::Blocked)];
    snapshot
}

fn state(layout: SidebarLayoutConfig, delivery: crate::config::ToastDelivery) -> ClientShellState {
    let mut config = Config::default();
    config.ui.sidebar_layout = layout;
    let mut config = ClientShellConfig::from_config(&config);
    config.toast_delivery = delivery;
    config.toast_delay_seconds = 0;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(background_snapshot()));
    state
}

fn server_event(pane_id: Option<&str>, tab_id: Option<&str>) -> SemanticNotification {
    SemanticNotification {
        kind: SemanticNotificationKind::NeedsAttention,
        title: "claude needs attention".into(),
        body: Some("leap-bi-4 · 1 · level plan".into()),
        sound: None,
        agent: Some("claude".into()),
        workspace_id: Some("ws_1".into()),
        tab_id: tab_id.map(str::to_owned),
        pane_id: pane_id.map(str::to_owned),
        position: None,
    }
}

fn delivered(
    state: &mut ClientShellState,
    event: SemanticNotification,
) -> (String, Option<String>) {
    state.receive_notification(&ClientEndpointId::Local, event, std::time::Instant::now());
    let card = state.visible_notifications.back().expect("card");
    (card.event.title.clone(), card.event.body.clone())
}

#[test]
fn tabs_layout_names_the_tab_then_agent_and_directory() {
    let mut state = state(
        SidebarLayoutConfig::Tabs,
        crate::config::ToastDelivery::Herdr,
    );
    assert_eq!(
        delivered(&mut state, server_event(Some("pane_2"), Some("tab_2"))),
        (
            "level plan needs attention".to_string(),
            Some("claude · leap-bi-4".to_string())
        )
    );

    // The pane alone resolves the tab too (no workspace id either, or the
    // focused space would suppress the card).
    let mut state = self::state(
        SidebarLayoutConfig::Tabs,
        crate::config::ToastDelivery::Herdr,
    );
    let mut event = server_event(Some("pane_2"), None);
    event.workspace_id = None;
    assert_eq!(delivered(&mut state, event).0, "level plan needs attention");
}

#[test]
fn tab_it_cannot_resolve_keeps_the_server_text() {
    let mut state = state(
        SidebarLayoutConfig::Tabs,
        crate::config::ToastDelivery::Herdr,
    );
    let mut event = server_event(Some("pane_2"), Some("tab_gone"));
    event.pane_id = Some("pane_2".into());
    assert_eq!(
        delivered(&mut state, event),
        (
            "claude needs attention".to_string(),
            Some("leap-bi-4 · 1 · level plan".to_string())
        )
    );
}

#[test]
fn spaces_layout_keeps_the_server_text() {
    let mut state = state(
        SidebarLayoutConfig::Spaces,
        crate::config::ToastDelivery::Herdr,
    );
    assert_eq!(
        delivered(&mut state, server_event(Some("pane_2"), Some("tab_2"))),
        (
            "claude needs attention".to_string(),
            Some("leap-bi-4 · 1 · level plan".to_string())
        )
    );
}

#[test]
fn directory_falls_back_to_the_space_label() {
    let mut state = state(
        SidebarLayoutConfig::Tabs,
        crate::config::ToastDelivery::Herdr,
    );
    let mut snapshot = background_snapshot();
    snapshot.workspaces[0].label = "backend".into();
    snapshot.panes[1].cwd = None;
    state.set_snapshot(Box::new(snapshot));
    assert_eq!(
        delivered(&mut state, server_event(Some("pane_2"), Some("tab_2"))).1,
        Some("claude · backend".to_string())
    );
}

#[test]
fn terminal_and_system_effects_carry_the_same_text() {
    use crate::config::ToastDelivery;
    for delivery in [ToastDelivery::Terminal, ToastDelivery::System] {
        let mut state = state(SidebarLayoutConfig::Tabs, delivery);
        let (effects, _) = state.receive_notification(
            &ClientEndpointId::Local,
            server_event(Some("pane_2"), Some("tab_2")),
            std::time::Instant::now(),
        );
        let texts = effects
            .iter()
            .filter_map(|effect| match effect {
                ClientShellNotificationEffect::Terminal { title, body }
                | ClientShellNotificationEffect::System { title, body } => {
                    Some((title.clone(), body.clone()))
                }
                ClientShellNotificationEffect::Sound { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            [(
                "level plan needs attention".to_string(),
                Some("claude · leap-bi-4".to_string())
            )],
            "{delivery:?}"
        );
    }
}

#[test]
fn mobile_banner_shows_the_rewritten_title() {
    let mut state = state(
        SidebarLayoutConfig::Tabs,
        crate::config::ToastDelivery::Herdr,
    );
    delivered(&mut state, server_event(Some("pane_2"), Some("tab_2")));
    let palette = state.config.palette.clone();
    let area = Rect::new(0, 0, 60, 10);
    let mut buffer = Buffer::empty(area);
    let card = state.visible_notifications.back().expect("card");
    super::super::notifications::render_mobile_notification_banner(
        &mut buffer,
        area,
        card,
        false,
        &palette,
    );
    let text = buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("level plan waiting"), "{text}");
    assert!(text.contains("claude · leap-bi-4"), "{text}");
}
