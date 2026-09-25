//! `ui.toast.herdr.sticky`: stacked in-app toast cards that stay until cleared.

use super::*;

fn sticky_state(max_stack: usize) -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Herdr;
    config.toast_delay_seconds = 0;
    config.toast_sticky = true;
    config.toast_max_stack = max_stack;
    let mut state = ClientShellState::new(config);
    state.sidebar_collapsed = true;
    state
}

fn agent(pane_id: &str, tab_id: &str, status: AgentStatus) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: tab_id.into(),
        name: None,
        display_agent: Some("codex".into()),
        agent: Some("codex".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }
}

/// The fixture snapshot (focused on tab_1) plus one agent per `(pane, tab, status)`.
fn snapshot_with(agents: &[(&str, &str, AgentStatus)]) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    for (pane_id, tab_id, status) in agents {
        snapshot.agents.push(agent(pane_id, tab_id, *status));
    }
    snapshot
}

fn event(
    kind: SemanticNotificationKind,
    title: &str,
    pane_id: Option<&str>,
    tab_id: Option<&str>,
) -> SemanticNotification {
    SemanticNotification {
        kind,
        title: title.into(),
        body: None,
        sound: None,
        agent: None,
        workspace_id: tab_id.map(|_| "ws_1".into()),
        tab_id: tab_id.map(str::to_owned),
        pane_id: pane_id.map(str::to_owned),
        position: None,
    }
}

fn custom(title: &str) -> SemanticNotification {
    event(SemanticNotificationKind::Custom, title, None, None)
}

fn attention(title: &str, pane_id: &str, tab_id: &str) -> SemanticNotification {
    event(
        SemanticNotificationKind::NeedsAttention,
        title,
        Some(pane_id),
        Some(tab_id),
    )
}

fn deliver(state: &mut ClientShellState, event: SemanticNotification) {
    state.receive_notification(&ClientEndpointId::Local, event, std::time::Instant::now());
}

fn titles(state: &ClientShellState) -> Vec<&str> {
    state
        .visible_notifications
        .iter()
        .map(|card| card.event.title.as_str())
        .collect()
}

fn mouse(kind: MouseEventKind, rect: Rect) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind,
        column: rect.x + 1,
        row: rect.y + 1,
        modifiers: KeyModifiers::empty(),
    })
}

fn card_hit(state: &ClientShellState, title: &str) -> Rect {
    let index = state
        .visible_notifications
        .iter()
        .position(|card| card.event.title == title)
        .expect("card");
    state
        .hits
        .notification_toasts
        .iter()
        .find(|(_, hit)| *hit == Some(index))
        .map(|(rect, _)| *rect)
        .expect("card hit")
}

fn focused_pane(outcome: &ClientShellInput) -> Option<String> {
    outcome.actions.iter().find_map(|action| match action {
        ClientShellAction::Endpoint { request, .. } => match &request.method {
            crate::api::schema::Method::PaneFocus(params) => Some(params.pane_id.clone()),
            _ => None,
        },
        _ => None,
    })
}

#[test]
fn per_event_position_stacks_in_its_own_corner() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut top = custom("top");
    top.position = Some(crate::config::ToastHerdrPosition::TopLeft);
    deliver(&mut state, custom("bottom"));
    deliver(&mut state, top);
    state.compose(100, 40).expect("frame");
    assert_eq!(
        state.hits.notification_toasts[0],
        (Rect::new(0, 0, 9, 3), Some(1))
    );
    // The top-left card does not push the bottom-right stack.
    assert_eq!(
        state.hits.notification_toasts[1],
        (Rect::new(88, 37, 12, 3), Some(0))
    );
}

#[test]
fn cards_beyond_max_stack_fold_into_one_more_line() {
    let mut state = sticky_state(3);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    for title in ["first", "second", "third", "fourth"] {
        deliver(&mut state, custom(title));
    }
    let frame = state.compose(100, 40).expect("frame");
    let hits = &state.hits.notification_toasts;
    assert_eq!(hits.len(), 4);
    assert_eq!(
        hits.iter().map(|(_, index)| *index).collect::<Vec<_>>(),
        [Some(3), Some(2), Some(1), None]
    );
    assert_eq!(hits[3].0, Rect::new(91, 27, 9, 1));
    let rows = frame_rows(&frame);
    assert!(rows[27].contains("+1 more"), "{}", rows[27]);
    assert!(!rows.iter().any(|row| row.contains("first")));

    // Clicks on the fold line are swallowed and change nothing.
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 95,
        row: 27,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(outcome.actions.is_empty());
    assert_eq!(state.visible_notifications.len(), 4);
}

#[test]
fn stack_height_is_capped_at_half_the_frame() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    for title in ["first", "second", "third", "fourth"] {
        deliver(&mut state, custom(title));
    }
    // 20 rows: a 10-row budget holds two cards (3 + 1 + 3) plus the fold line.
    let frame = state.compose(100, 20).expect("frame");
    let hits = &state.hits.notification_toasts;
    assert_eq!(
        hits.iter().map(|(_, index)| *index).collect::<Vec<_>>(),
        [Some(3), Some(2), None]
    );
    assert!(hits.iter().all(|(rect, _)| rect.y >= 10));
    assert!(frame_rows(&frame)[11].contains("+2 more"));
}

#[test]
fn left_click_focuses_the_target_and_removes_only_that_card() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot_with(&[
        ("pane_2", "tab_2", AgentStatus::Blocked),
        ("pane_3", "tab_3", AgentStatus::Blocked),
    ])));
    state.set_pane_surface(surface());
    deliver(&mut state, attention("two", "pane_2", "tab_2"));
    deliver(&mut state, attention("three", "pane_3", "tab_3"));
    state.compose(100, 40).expect("frame");

    let hit = card_hit(&state, "two");
    let outcome =
        state.handle_raw_events(vec![mouse(MouseEventKind::Down(MouseButton::Left), hit)]);

    assert_eq!(focused_pane(&outcome).as_deref(), Some("pane_2"));
    assert_eq!(titles(&state), ["three"]);
}

#[test]
fn right_click_dismisses_a_card_without_focusing() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot_with(&[
        ("pane_2", "tab_2", AgentStatus::Blocked),
        ("pane_3", "tab_3", AgentStatus::Blocked),
    ])));
    state.set_pane_surface(surface());
    deliver(&mut state, attention("two", "pane_2", "tab_2"));
    deliver(&mut state, attention("three", "pane_3", "tab_3"));
    state.compose(100, 40).expect("frame");

    let hit = card_hit(&state, "three");
    let outcome =
        state.handle_raw_events(vec![mouse(MouseEventKind::Down(MouseButton::Right), hit)]);

    assert!(focused_pane(&outcome).is_none());
    assert!(outcome.repaint);
    assert_eq!(titles(&state), ["two"]);
}

#[test]
fn focusing_the_target_tab_clears_its_card_on_the_next_snapshot() {
    let mut state = sticky_state(6);
    let agents = [
        ("pane_2", "tab_2", AgentStatus::Blocked),
        ("pane_3", "tab_3", AgentStatus::Blocked),
    ];
    state.set_snapshot(Box::new(snapshot_with(&agents)));
    deliver(&mut state, attention("two", "pane_2", "tab_2"));
    deliver(&mut state, attention("three", "pane_3", "tab_3"));

    let mut focused = snapshot_with(&agents);
    focused.revision += 1;
    focused.focused_tab_id = Some("tab_2".into());
    state.set_snapshot(Box::new(focused));

    assert_eq!(titles(&state), ["three"]);
}

#[test]
fn stale_cards_clear_on_the_next_snapshot() {
    let mut state = sticky_state(6);
    let mut initial = snapshot_with(&[
        ("pane_2", "tab_2", AgentStatus::Blocked),
        ("pane_3", "tab_3", AgentStatus::Working),
        ("pane_4", "tab_4", AgentStatus::Working),
        ("pane_5", "tab_5", AgentStatus::Blocked),
        ("pane_6", "tab_6", AgentStatus::Blocked),
    ]);
    initial.panes.push(ClientShellPane {
        pane_id: "pane_6".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_6".into(),
        label: None,
        cwd: None,
        foreground_cwd: None,
        focused: false,
        right_click_passthrough: false,
    });
    state.set_snapshot(Box::new(initial.clone()));
    // The client projects Working -> Idle as an unseen Done.
    let mut done = initial;
    done.revision += 1;
    for agent in &mut done.agents[1..3] {
        agent.agent_status = AgentStatus::Idle;
        agent.state_change_seq = 2;
    }
    state.set_snapshot(Box::new(done.clone()));
    deliver(&mut state, attention("answered", "pane_2", "tab_2"));
    for (title, pane, tab) in [
        ("restarted", "pane_3", "tab_3"),
        ("still done", "pane_4", "tab_4"),
    ] {
        deliver(
            &mut state,
            event(
                SemanticNotificationKind::Finished,
                title,
                Some(pane),
                Some(tab),
            ),
        );
    }
    deliver(&mut state, attention("closed", "pane_5", "tab_5"));
    deliver(&mut state, attention("agent exited", "pane_6", "tab_6"));
    assert_eq!(
        titles(&state),
        [
            "answered",
            "restarted",
            "still done",
            "closed",
            "agent exited"
        ]
    );

    let mut next = done;
    next.revision += 1;
    // NeedsAttention whose agent is no longer blocked.
    next.agents[0].agent_status = AgentStatus::Working;
    next.agents[0].state_change_seq = 3;
    // Finished whose agent started working again.
    next.agents[1].agent_status = AgentStatus::Working;
    next.agents[1].state_change_seq = 3;
    // pane_5 is gone; pane_6 keeps its pane but lost its agent (awaiting: keep).
    next.agents
        .retain(|agent| agent.pane_id != "pane_5" && agent.pane_id != "pane_6");
    state.set_snapshot(Box::new(next));

    assert_eq!(titles(&state), ["still done", "agent exited"]);
}

#[test]
fn newer_event_for_the_same_pane_replaces_the_older_card() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot_with(&[
        ("pane_2", "tab_2", AgentStatus::Blocked),
        ("pane_3", "tab_3", AgentStatus::Blocked),
    ])));
    deliver(&mut state, attention("old", "pane_2", "tab_2"));
    deliver(&mut state, attention("other", "pane_3", "tab_3"));

    deliver(&mut state, attention("new", "pane_2", "tab_2"));

    assert_eq!(titles(&state), ["other", "new"]);
}

#[test]
fn retiring_an_endpoint_drops_only_its_cards() {
    let mut state = sticky_state(6);
    let remote_id = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    for (endpoint_id, title) in [
        (ClientEndpointId::Local, "local one"),
        (remote_id.clone(), "remote"),
        (ClientEndpointId::Local, "local two"),
    ] {
        state
            .visible_notifications
            .push_back(ClientVisibleNotification {
                endpoint_id,
                event: custom(title),
                deadline: std::time::Instant::now(),
            });
    }

    state.retire_endpoint_notifications(&ClientEndpointId::Local);

    assert_eq!(titles(&state), ["remote"]);
}

#[test]
fn open_notification_target_focuses_the_newest_card() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot_with(&[
        ("pane_2", "tab_2", AgentStatus::Blocked),
        ("pane_3", "tab_3", AgentStatus::Blocked),
    ])));
    deliver(&mut state, attention("two", "pane_2", "tab_2"));
    deliver(&mut state, attention("three", "pane_3", "tab_3"));

    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenNotificationTarget),
        &mut outcome,
    );

    assert_eq!(focused_pane(&outcome).as_deref(), Some("pane_3"));
    assert_eq!(titles(&state), ["two"]);
}

#[test]
fn focused_tab_events_are_still_suppressed() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot_with(&[(
        "pane_1",
        "tab_1",
        AgentStatus::Blocked,
    )])));

    deliver(&mut state, attention("focused", "pane_1", "tab_1"));

    assert!(state.visible_notifications.is_empty());
}

#[test]
fn occlusion_covers_every_card_and_the_fold_line() {
    let mut state = sticky_state(2);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    for title in ["first", "second", "third"] {
        deliver(&mut state, custom(title));
    }
    state.compose(100, 40).expect("frame");
    let rects = state
        .hits
        .notification_toasts
        .iter()
        .map(|(rect, _)| *rect)
        .collect::<Vec<_>>();
    assert_eq!(rects.len(), 3);
    for rect in rects {
        super::graphics::assert_graphics_cover(&mut state, rect, 100, 40);
    }
}

#[test]
fn mobile_banner_shows_only_the_newest_card() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    for title in ["older", "newest"] {
        deliver(&mut state, custom(title));
    }
    let frame = state.compose(44, 30).expect("mobile frame");
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("newest"));
    assert!(!text.contains("older"));
    assert_eq!(state.hits.notification_toasts.len(), 1);
    assert_eq!(state.hits.notification_toasts[0].1, Some(1));
}

fn row_patch(x: u16, y: u16) -> crate::protocol::PaneSurfacePatch {
    let mut pane = surface().panes[0].clone();
    pane.content_revision = 1;
    crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x,
            y,
            cells: vec![
                crate::protocol::CellData {
                    symbol: "$".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                };
                2
            ],
        }],
        panes: vec![pane],
        cursor: None,
    }
}

#[test]
fn sticky_cards_block_the_fast_path_only_where_they_are_drawn() {
    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    deliver(&mut state, custom("corner"));
    state.compose(100, 40).expect("frame");
    // The bottom-right card is far from the 4x2 pane at the surface origin.
    assert!(matches!(
        state.apply_pane_surface_patch(row_patch(0, 0)),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));

    let mut state = sticky_state(6);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut over_pane = custom("over pane");
    over_pane.position = Some(crate::config::ToastHerdrPosition::TopLeft);
    deliver(&mut state, over_pane);
    state.compose(100, 40).expect("frame");
    let area = state.layout(100, 40).pane_surface;
    let card = state.hits.notification_toasts[0].0;
    assert!(card.intersects(Rect::new(area.x, area.y + 1, 2, 1)));
    assert!(matches!(
        state.apply_pane_surface_patch(row_patch(0, 1)),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));
}

#[test]
fn turning_sticky_off_keeps_one_timed_card_and_queues_the_rest() {
    let mut state = sticky_state(6);
    for title in ["first", "second", "third"] {
        deliver(&mut state, custom(title));
    }
    state.config.toast_sticky = false;
    state.rebalance_notification_cards(std::time::Instant::now());
    assert_eq!(titles(&state), ["first"]);
    assert_eq!(
        state
            .queued_notifications
            .iter()
            .map(|card| card.event.title.as_str())
            .collect::<Vec<_>>(),
        ["second", "third"]
    );

    state.config.toast_sticky = true;
    state.rebalance_notification_cards(std::time::Instant::now());
    assert_eq!(titles(&state), ["first", "second", "third"]);
    assert!(state.queued_notifications.is_empty());
}

mod fork_smoke {
    use super::*;

    #[test]
    fn three_cards_stack_newest_nearest_the_corner() {
        let mut state = sticky_state(6);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        for title in ["first", "second", "third"] {
            deliver(&mut state, custom(title));
        }
        assert_eq!(titles(&state), ["first", "second", "third"]);
        // Timed-mode fields stay unused.
        assert!(state.queued_notifications.is_empty());

        let frame = state.compose(100, 40).expect("frame");
        // Bottom-right default, 3-row cards, one blank row between them.
        assert_eq!(
            state.hits.notification_toasts,
            vec![
                (Rect::new(89, 37, 11, 3), Some(2)),
                (Rect::new(88, 33, 12, 3), Some(1)),
                (Rect::new(89, 29, 11, 3), Some(0)),
            ]
        );
        let rows = frame_rows(&frame);
        assert!(rows[38].contains("third"));
        assert!(rows[34].contains("second"));
        assert!(rows[30].contains("first"));
        assert!(!rows[36].contains('─') && !rows[32].contains('─'));

        // Sticky cards have no deadline.
        let (_, repaint) = state
            .tick_notifications(std::time::Instant::now() + std::time::Duration::from_secs(60));
        assert!(!repaint);
        assert_eq!(state.visible_notifications.len(), 3);
    }
}
