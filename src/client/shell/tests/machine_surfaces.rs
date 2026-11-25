//! Fork (machine tabs): toasts, cards, reminders, notes, team, News,
//! coordinator, settings and menus with more than one endpoint.
//!
//! Real servers number their spaces, tabs and panes alike and notes keys
//! follow them, so both machines of the fixture use the same ids on purpose;
//! only the boot id and the labels differ.

use super::*;
use crate::api::schema::coordinator::{CoordinatorGetInfo, CoordinatorStateInfo};
use crate::api::schema::notes::NotesInfo;
use crate::api::schema::{AgentNoticeInfo, AgentNoticeKind, Method, NewsGetInfo, ResponseResult};
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crate::client::shell::agent_cards::AgentCardHit;
use crate::client::shell::coordinator::CoordinatorRequestKind;
use crate::client::shell::info_dock::InfoRequest;
use crate::config::{SidebarLayoutConfig, ToastDelivery};
use crate::server::headless::agent_notices::AgentNoticesPayload;
use crate::server::headless::notes_reveal::NotesRevealPayload;
use crate::server::headless::tab_mutes::TabMutesPayload;
use std::time::{Duration, Instant};

const COLS: u16 = 150;
const ROWS: u16 = 40;
const LOCAL_BOOT: &str = "boot-1";
const REMOTE_BOOT: &str = "remote-boot";
const MINUTE: Duration = Duration::from_secs(60);

fn profile() -> SavedSshEndpoint {
    SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    }
}

fn build() -> ClientEndpointId {
    ClientEndpointId::Ssh(profile().id)
}

const LOCAL: ClientEndpointId = ClientEndpointId::Local;

fn boot(endpoint_id: &ClientEndpointId) -> &'static str {
    if endpoint_id.is_local() {
        LOCAL_BOOT
    } else {
        REMOTE_BOOT
    }
}

/// `tab_1` (focused, `pane_1`), `tab_2` (`pane_2`, a blocked agent) and
/// `tab_3` (`pane_3`).
fn machine_snapshot(endpoint_id: &ClientEndpointId, name: &str) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.boot_id = boot(endpoint_id).into();
    let tab = snapshot.tabs[0].clone();
    let pane = snapshot.panes[0].clone();
    snapshot.tabs = (1..=3)
        .map(|number| {
            let mut tab = tab.clone();
            tab.tab_id = format!("tab_{number}");
            tab.number = number;
            tab.label = format!("{name}-{number}");
            tab.custom_label = true;
            tab.focused = number == 1;
            if number == 2 {
                tab.agent_status = AgentStatus::Blocked;
            }
            tab
        })
        .collect();
    snapshot.panes = (1..=3)
        .map(|number| {
            let mut pane = pane.clone();
            pane.pane_id = format!("pane_{number}");
            pane.tab_id = format!("tab_{number}");
            pane.focused = number == 1;
            pane
        })
        .collect();
    snapshot.agents = vec![ClientShellAgent {
        pane_id: "pane_2".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_2".into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }];
    snapshot
}

fn shell_config() -> ClientShellConfig {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config.ui.sidebar_active_agents = false;
    let mut config = ClientShellConfig::from_config(&config);
    config.toast_delivery = ToastDelivery::Herdr;
    config.toast_delay_seconds = 0;
    config.toast_sticky = true;
    config.idle_reminder_minutes = 5;
    config.startup_onboarding = false;
    config
}

fn boot_surface(endpoint_id: &ClientEndpointId) -> PaneSurfaceFrame {
    let mut surface = surface();
    surface.boot_id = boot(endpoint_id).into();
    surface
}

fn one_machine() -> ClientShellState {
    let mut state = ClientShellState::new(shell_config());
    state.set_endpoint_snapshot(&LOCAL, Box::new(machine_snapshot(&LOCAL, "local")));
    state.set_pane_surface(boot_surface(&LOCAL));
    state.compose(COLS, ROWS).unwrap();
    state
}

/// Local shown, the saved machine `Build` online beside it.
fn two_machines() -> ClientShellState {
    let mut state = one_machine();
    state.set_endpoint_catalog(&[profile()]);
    state.set_endpoint_status(&build(), ClientEndpointStatus::Online);
    state.set_endpoint_snapshot(&build(), Box::new(machine_snapshot(&build(), "build")));
    state.compose(COLS, ROWS).unwrap();
    state
}

fn switch(state: &mut ClientShellState, endpoint_id: &ClientEndpointId) {
    assert!(state.activate_endpoint_projection(endpoint_id));
    state.set_pane_surface(boot_surface(endpoint_id));
    state.compose(COLS, ROWS).unwrap();
}

/// `Build` selected.
fn on_build() -> ClientShellState {
    let mut state = two_machines();
    switch(&mut state, &build());
    state
}

fn frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(COLS, ROWS).expect("composed frame").frame;
    frame
        .cells
        .chunks(usize::from(frame.width))
        .map(|row| row.iter().map(|cell| cell.symbol.as_str()).collect())
        .collect::<Vec<String>>()
        .join("\n")
}

fn sent(actions: &[ClientShellAction]) -> Vec<(ClientEndpointId, String, Method)> {
    actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint {
                endpoint_id,
                boot_id,
                request,
            } => {
                assert_eq!(boot_id, boot(endpoint_id), "the machine's own boot");
                Some((
                    endpoint_id.clone(),
                    request.id.clone(),
                    request.method.clone(),
                ))
            }
            _ => None,
        })
        .collect()
}

/// The method names sent to `endpoint_id`; nothing went elsewhere.
fn sent_to(outcome: &ClientShellInput, endpoint_id: &ClientEndpointId) -> Vec<String> {
    sent(&outcome.actions)
        .into_iter()
        .map(|(to, _, method)| {
            assert_eq!(&to, endpoint_id, "{method:?}");
            crate::api::api_method_name(&method).to_owned()
        })
        .collect()
}

fn answer(
    state: &mut ClientShellState,
    request_id: &str,
    result: ResponseResult,
) -> (bool, Vec<ClientShellAction>) {
    let boot_id = boot(&state.active_endpoint_id);
    state.handle_endpoint_result(boot_id, request_id, Ok(result))
}

// ---------------------------------------------------------------- toasts

/// The server's "needs attention" for `pane_2` (the same on both machines).
fn attention(pane: u32) -> SemanticNotification {
    SemanticNotification {
        kind: SemanticNotificationKind::NeedsAttention,
        title: "claude needs attention".into(),
        body: None,
        sound: Some(SemanticNotificationSound::Request),
        agent: Some("claude".into()),
        workspace_id: Some("ws_1".into()),
        tab_id: Some(format!("tab_{pane}")),
        pane_id: Some(format!("pane_{pane}")),
        position: None,
    }
}

fn toast(state: &mut ClientShellState, endpoint_id: &ClientEndpointId) {
    let (effects, repaint) = state.receive_notification(endpoint_id, attention(2), Instant::now());
    assert!(repaint, "{endpoint_id:?}");
    assert_eq!(effects.len(), 1, "the sound");
}

fn titles(state: &ClientShellState) -> Vec<&str> {
    state
        .visible_notifications
        .iter()
        .map(|card| card.event.title.as_str())
        .collect()
}

fn activation(outcome: &ClientShellInput) -> Option<(ClientEndpointId, String)> {
    match outcome.actions.as_slice() {
        [ClientShellAction::ActivateEndpoint {
            endpoint_id,
            target: Some(ClientEndpointFocusTarget::Pane(pane)),
        }] => Some((endpoint_id.clone(), pane.clone())),
        _ => None,
    }
}

#[test]
fn toasts_name_their_machine_once_and_only_with_several() {
    let mut state = two_machines();
    toast(&mut state, &LOCAL);
    toast(&mut state, &build());
    assert_eq!(
        titles(&state),
        [
            "Local · local-2 needs attention",
            "Build · build-2 needs attention"
        ],
        "the same pane id: two toasts"
    );
    let text = frame_text(&mut state);
    assert_eq!(
        text.matches("Build · build-2 needs attention").count(),
        1,
        "{text}"
    );
    // The machine shown is named too: it may be another one by the time the
    // toast is read.
    switch(&mut state, &build());
    toast(&mut state, &build());
    assert_eq!(
        titles(&state).last(),
        Some(&"Build · build-2 needs attention")
    );
    // A notification the server phrased itself.
    let custom = SemanticNotification {
        kind: SemanticNotificationKind::Custom,
        title: "deploy".into(),
        pane_id: None,
        tab_id: None,
        workspace_id: None,
        ..attention(2)
    };
    state.receive_notification(&build(), custom, Instant::now());
    assert_eq!(titles(&state).last(), Some(&"Build · deploy"));

    let mut state = one_machine();
    toast(&mut state, &LOCAL);
    assert_eq!(titles(&state), ["local-2 needs attention"]);
}

#[test]
fn system_notifications_name_their_machine() {
    for (several, expected) in [
        (true, "Build · build-2 needs attention"),
        (false, "local-2 needs attention"),
    ] {
        let (mut state, endpoint_id) = if several {
            (two_machines(), build())
        } else {
            (one_machine(), LOCAL)
        };
        state.config.toast_delivery = ToastDelivery::System;
        let (effects, _) = state.receive_notification(&endpoint_id, attention(2), Instant::now());
        let title = effects.iter().find_map(|effect| match effect {
            ClientShellNotificationEffect::System { title, .. } => Some(title.as_str()),
            _ => None,
        });
        assert_eq!(title, Some(expected));
    }
}

#[test]
fn a_toast_click_goes_to_its_machines_pane() {
    let mut state = two_machines();
    toast(&mut state, &LOCAL);
    toast(&mut state, &build());
    let mut outcome = ClientShellInput::default();
    state.focus_notification_at(1, &mut outcome);
    assert_eq!(activation(&outcome), Some((build(), "pane_2".into())));
    assert_eq!(titles(&state), ["Local · local-2 needs attention"]);
    // Local's, Local shown: through the runtime, so it cancels a switch
    // under way like a row click does.
    let mut outcome = ClientShellInput::default();
    state.focus_notification_at(0, &mut outcome);
    assert_eq!(activation(&outcome), Some((LOCAL, "pane_2".into())));

    let mut state = on_build();
    toast(&mut state, &LOCAL);
    toast(&mut state, &build());
    let mut outcome = ClientShellInput::default();
    state.focus_notification_at(1, &mut outcome);
    assert_eq!(sent_to(&outcome, &build()), ["pane.focus"]);
    let mut outcome = ClientShellInput::default();
    state.focus_notification_at(0, &mut outcome);
    assert_eq!(activation(&outcome), Some((LOCAL, "pane_2".into())));

    let mut state = one_machine();
    toast(&mut state, &LOCAL);
    let mut outcome = ClientShellInput::default();
    state.focus_notification_at(0, &mut outcome);
    assert_eq!(sent_to(&outcome, &LOCAL), ["pane.focus"]);
}

#[test]
fn a_toast_of_a_machine_that_is_away_stays_and_answers_every_click() {
    let mut state = two_machines();
    toast(&mut state, &build());
    state.mark_endpoint_disconnected(&build());
    for click in 0..2 {
        state.visible_endpoint_notice = None;
        let mut outcome = ClientShellInput::default();
        state.focus_notification_at(0, &mut outcome);
        assert!(outcome.actions.is_empty());
        assert_eq!(titles(&state).len(), 1);
        let notice = state
            .visible_endpoint_notice
            .as_ref()
            .unwrap_or_else(|| panic!("click {click} is answered"));
        assert_eq!(notice.body, "Build is unavailable");
    }
    // Turned off or removed: its toasts go, Local's stay.
    toast(&mut state, &LOCAL);
    state.retire_endpoint(&build());
    assert_eq!(titles(&state), ["Local · local-2 needs attention"]);
}

#[test]
fn showing_a_machine_clears_the_toast_of_its_focused_tab() {
    let mut state = two_machines();
    for endpoint_id in [&LOCAL, &build()] {
        let mut snapshot = machine_snapshot(endpoint_id, "again");
        snapshot.revision = 2;
        snapshot.focused_tab_id = Some("tab_2".into());
        if endpoint_id.is_local() {
            // Local looks at another tab: its own toast must stay.
            snapshot.focused_tab_id = Some("tab_1".into());
        }
        state.set_endpoint_snapshot(endpoint_id, Box::new(snapshot));
    }
    toast(&mut state, &LOCAL);
    toast(&mut state, &build());
    assert_eq!(titles(&state).len(), 2);
    assert!(state.activate_endpoint_projection(&build()));
    assert_eq!(
        titles(&state),
        ["Local · again-2 needs attention"],
        "without a further snapshot of Build"
    );
}

// ----------------------------------------------------------- agent cards

fn card(id: &str) -> AgentNoticeInfo {
    AgentNoticeInfo {
        id: id.into(),
        kind: AgentNoticeKind::Question,
        title: format!("title {id}"),
        name: "fixer".into(),
        pane_id: "pane_2".into(),
        tab_id: Some("tab_2".into()),
        tab_label: Some("a tab".into()),
        unix: 1_800_000_000,
        ..AgentNoticeInfo::default()
    }
}

fn push_cards(
    state: &mut ClientShellState,
    endpoint_id: &ClientEndpointId,
    revision: u64,
    ids: &[&str],
) -> Vec<ClientShellNotificationEffect> {
    state
        .receive_agent_notices(
            endpoint_id,
            AgentNoticesPayload {
                boot_id: boot(endpoint_id).into(),
                revision,
                notices: ids.iter().map(|id| card(id)).collect(),
                initial: false,
            },
        )
        .0
}

fn shown_cards(state: &ClientShellState) -> Vec<(ClientEndpointId, &str)> {
    state
        .agent_card_views()
        .iter()
        .map(|view| (view.endpoint_id.clone(), view.notice.id.as_str()))
        .collect()
}

/// Both machines' card `n1`, pushed after a first (seeding) payload.
fn two_cards() -> ClientShellState {
    let mut state = two_machines();
    for endpoint_id in [LOCAL, build()] {
        push_cards(&mut state, &endpoint_id, 1, &[]);
        push_cards(&mut state, &endpoint_id, 2, &["n1"]);
    }
    state
}

#[test]
fn cards_name_their_machine_and_stay_apart() {
    let mut state = two_cards();
    assert_eq!(shown_cards(&state), [(LOCAL, "n1"), (build(), "n1")]);
    let text = frame_text(&mut state);
    for machine in ["Local", "Build"] {
        let header = format!("? {machine} · fixer · a tab");
        assert_eq!(text.matches(&header).count(), 1, "{text}");
    }
    let hits: Vec<_> = state
        .hits
        .agent_cards
        .iter()
        .filter_map(|(_, hit)| match hit {
            AgentCardHit::Card { endpoint_id, .. } => Some(endpoint_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(hits, [LOCAL, build()]);

    let mut state = one_machine();
    push_cards(&mut state, &LOCAL, 1, &[]);
    push_cards(&mut state, &LOCAL, 2, &["n1"]);
    let text = frame_text(&mut state);
    assert!(text.contains("? fixer · a tab"), "{text}");
    assert!(!text.contains("Local ·"), "{text}");
}

#[test]
fn a_narrow_card_and_the_phone_banner_name_the_machine() {
    let notice = AgentNoticeInfo {
        team: Some("crew".into()),
        ..card("n1")
    };
    let endpoint_id = build();
    let views = [super::super::agent_cards::AgentCardView {
        endpoint_id: &endpoint_id,
        machine: Some("a-machine-with-a-long-name"),
        notice: &notice,
    }];
    let palette = crate::app::client_palette_from_config(&Config::default());
    let area = Rect::new(0, 0, 30, 8);
    let mut buffer = Buffer::empty(area);
    super::super::agent_cards::render_agent_cards(
        &mut buffer,
        area,
        0,
        &views,
        notice.unix,
        &palette,
    );
    let header: String = (0..30)
        .map(|x| buffer[(x, 1)].symbol().to_owned())
        .collect();
    assert!(header.contains("… · fixer"), "{header}");
    assert!(header.contains('×'), "{header}");

    let area = Rect::new(0, 0, 90, 4);
    let mut buffer = Buffer::empty(area);
    super::super::agent_cards::render_agent_card_banner(&mut buffer, area, &views, false, &palette);
    let line: String = (0..90)
        .map(|x| buffer[(x, 3)].symbol().to_owned())
        .collect();
    assert!(
        line.contains("a-machine-with-a-long-name · ◆ crew · fixer: title n1"),
        "{line}"
    );
    let diamond = (0..90).find(|x| buffer[(*x, 3)].symbol() == "◆").unwrap();
    assert_eq!(buffer[(diamond, 3)].fg, palette.accent);
}

#[test]
fn a_cards_system_notification_names_the_machine() {
    let mut state = two_machines();
    state.config.toast_delivery = ToastDelivery::System;
    push_cards(&mut state, &build(), 1, &[]);
    let effects = push_cards(&mut state, &build(), 2, &["n1"]);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        ClientShellNotificationEffect::System { title, .. } if title == "Build · fixer: title n1"
    )));
    let mut state = one_machine();
    state.config.toast_delivery = ToastDelivery::System;
    push_cards(&mut state, &LOCAL, 1, &[]);
    let effects = push_cards(&mut state, &LOCAL, 2, &["n1"]);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        ClientShellNotificationEffect::System { title, .. } if title == "fixer: title n1"
    )));
}

fn click_card(state: &mut ClientShellState, endpoint_id: &ClientEndpointId) -> ClientShellInput {
    state.compose(COLS, ROWS).unwrap();
    let rect = state
        .hits
        .agent_cards
        .iter()
        .find_map(|(rect, hit)| match hit {
            AgentCardHit::Card {
                endpoint_id: to, ..
            } if to == endpoint_id => Some(*rect),
            _ => None,
        })
        .expect("the machine's card is drawn");
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_agent_card_mouse(
        MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x + 4,
            row: rect.y + 1,
            modifiers: crossterm::event::KeyModifiers::empty(),
        },
        &mut outcome
    ));
    outcome
}

fn tick_machines(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_machines(Instant::now(), &mut outcome);
    outcome
}

/// The request id and the cards of the dismissal a tick `later` sends.
fn dismissal(state: &mut ClientShellState, later: Duration) -> Option<(String, Vec<String>)> {
    let mut outcome = ClientShellInput::default();
    state.tick_machines(Instant::now() + later, &mut outcome);
    match &sent(&outcome.actions)[..] {
        [] => None,
        [(to, id, Method::AgentNoticeDismiss(params))] => {
            assert_eq!(to, &state.active_endpoint_id);
            assert!(!params.all);
            Some((id.clone(), params.ids.clone()))
        }
        other => panic!("{other:?}"),
    }
}

/// Build's card `n1` dismissed while Local is shown, then Build shown.
fn deferred_dismissal() -> ClientShellState {
    let mut state = two_cards();
    let mut outcome = ClientShellInput::default();
    state.dismiss_agent_card(&build(), "n1".into(), &mut outcome);
    switch(&mut state, &build());
    state
}

#[test]
fn a_card_click_goes_to_its_machines_pane() {
    let mut state = two_cards();
    let outcome = click_card(&mut state, &build());
    assert_eq!(activation(&outcome), Some((build(), "pane_2".into())));
    let outcome = click_card(&mut state, &LOCAL);
    assert_eq!(activation(&outcome), Some((LOCAL, "pane_2".into())));
    assert_eq!(shown_cards(&state).len(), 2, "the visit clears them");

    // The window on Build is on that tab already, so the visit cannot: the
    // server is told once Build is shown.
    let mut snapshot = machine_snapshot(&build(), "build");
    snapshot.revision = 2;
    snapshot.focused_tab_id = Some("tab_2".into());
    state.set_endpoint_snapshot(&build(), Box::new(snapshot));
    let outcome = click_card(&mut state, &build());
    assert_eq!(activation(&outcome), Some((build(), "pane_2".into())));
    assert_eq!(shown_cards(&state).len(), 2, "a switch can fail");
    assert!(
        tick_machines(&mut state).actions.is_empty(),
        "Local is shown"
    );
    assert!(state.activate_endpoint_projection(&build()));
    let outcome = tick_machines(&mut state);
    assert_eq!(sent_to(&outcome, &build()), ["agent.notice_dismiss"]);
    assert_eq!(shown_cards(&state), [(LOCAL, "n1")]);
}

#[test]
fn dismissing_another_machines_card_reaches_its_server_once_shown() {
    let mut state = two_cards();
    let mut outcome = ClientShellInput::default();
    state.dismiss_agent_card(&build(), "n1".into(), &mut outcome);
    assert!(
        outcome.actions.is_empty(),
        "requests reach the shown machine"
    );
    assert_eq!(shown_cards(&state), [(LOCAL, "n1")]);
    assert!(tick_machines(&mut state).actions.is_empty());
    // Its next list still has the card: it stays hidden.
    push_cards(&mut state, &build(), 3, &["n1"]);
    assert_eq!(shown_cards(&state), [(LOCAL, "n1")]);

    switch(&mut state, &build());
    let outcome = tick_machines(&mut state);
    let [(to, _, Method::AgentNoticeDismiss(params))] = &sent(&outcome.actions)[..] else {
        panic!("{:?}", outcome.actions);
    };
    assert_eq!(to, &build());
    assert_eq!(
        (params.ids.as_slice(), params.all),
        (&["n1".into()][..], false)
    );
    assert!(tick_machines(&mut state).actions.is_empty(), "once");
    assert_eq!(shown_cards(&state), [(LOCAL, "n1")]);
}

/// The runtime cancels a machine's queued requests when it is left or lost.
#[test]
fn a_cancelled_dismissal_is_sent_again() {
    let mut state = deferred_dismissal();
    let (id, ids) = dismissal(&mut state, Duration::ZERO).expect("sent once shown");
    assert_eq!(ids, ["n1"]);
    assert!(!state.cancel_endpoint_request(&id));
    assert!(state.visible_endpoint_notice.is_none(), "silent");
    assert_eq!(shown_cards(&state), [(LOCAL, "n1")], "still hidden");
    assert_eq!(dismissal(&mut state, Duration::ZERO), None, "not at once");
    let (_, ids) = dismissal(&mut state, Duration::from_secs(2)).expect("again");
    assert_eq!(ids, ["n1"]);

    // The machine goes offline before it answered.
    let mut state = deferred_dismissal();
    dismissal(&mut state, Duration::ZERO).expect("sent");
    state.mark_endpoint_disconnected(&build());
    assert_eq!(dismissal(&mut state, MINUTE), None, "away");
    state.set_endpoint_status(&build(), ClientEndpointStatus::Online);
    assert!(dismissal(&mut state, MINUTE).is_some(), "back");
}

#[test]
fn a_failed_or_timed_out_dismissal_is_sent_again_later() {
    for code in ["endpoint_timeout", "server_unavailable", "internal"] {
        let mut state = deferred_dismissal();
        let (id, _) = dismissal(&mut state, Duration::ZERO).expect("sent");
        let (repaint, actions) = state.handle_endpoint_result(
            REMOTE_BOOT,
            &id,
            Err(ClientShellEndpointError {
                code: Some(code.into()),
                message: "no".into(),
            }),
        );
        assert!(!repaint && actions.is_empty(), "{code}");
        assert!(state.visible_endpoint_notice.is_none(), "{code}");
        assert_eq!(
            dismissal(&mut state, Duration::from_secs(2)),
            None,
            "{code}: no busy loop"
        );
        assert!(dismissal(&mut state, MINUTE).is_some(), "{code}");
    }
}

#[test]
fn a_dismissal_cut_off_by_a_switch_is_sent_when_its_machine_is_back() {
    // Cancelled before or after the other machine is shown, or not at all.
    for cancelled in [Some(true), Some(false), None] {
        let mut state = deferred_dismissal();
        let (id, _) = dismissal(&mut state, Duration::ZERO).expect("sent");
        if cancelled == Some(true) {
            state.cancel_endpoint_request(&id);
        }
        switch(&mut state, &LOCAL);
        if cancelled == Some(false) {
            state.cancel_endpoint_request(&id);
        }
        assert_eq!(dismissal(&mut state, MINUTE), None, "Local's was not");
        switch(&mut state, &build());
        let (_, ids) = dismissal(&mut state, MINUTE).expect("sent again");
        assert_eq!(ids, ["n1"], "{cancelled:?}");
    }
    // The reply of the machine left comes while the other is shown, its
    // request still known (a runtime that did not cancel it): the boot check
    // must not eat the failure.
    let mut state = deferred_dismissal();
    let (id, _) = dismissal(&mut state, Duration::ZERO).expect("sent");
    let pending = state.pending_requests.remove(&id).expect("pending");
    switch(&mut state, &LOCAL);
    state.agent_cards.get_mut(&build()).unwrap().unsent.clear();
    state.pending_requests.insert(id.clone(), pending);
    state.cancel_endpoint_request(&id);
    switch(&mut state, &build());
    assert!(dismissal(&mut state, MINUTE).is_some());
}

#[test]
fn a_dismissal_its_server_confirmed_is_not_sent_again() {
    // Its list came without the card before the request was cut off.
    let mut state = deferred_dismissal();
    let (id, _) = dismissal(&mut state, Duration::ZERO).expect("sent");
    push_cards(&mut state, &build(), 3, &[]);
    state.cancel_endpoint_request(&id);
    assert_eq!(dismissal(&mut state, MINUTE), None);
    // Or after it.
    let mut state = deferred_dismissal();
    let (id, _) = dismissal(&mut state, Duration::ZERO).expect("sent");
    state.cancel_endpoint_request(&id);
    push_cards(&mut state, &build(), 3, &[]);
    assert_eq!(dismissal(&mut state, MINUTE), None);
    // Or it answered.
    let mut state = deferred_dismissal();
    let (id, _) = dismissal(&mut state, Duration::ZERO).expect("sent");
    answer(
        &mut state,
        &id,
        ResponseResult::AgentNotices {
            notices: Vec::new(),
        },
    );
    switch(&mut state, &LOCAL);
    switch(&mut state, &build());
    assert_eq!(dismissal(&mut state, MINUTE), None);
}

/// Card ids start at `n1` on every boot.
#[test]
fn a_dismissal_never_reaches_a_restarted_server() {
    let mut state = deferred_dismissal();
    dismissal(&mut state, Duration::ZERO).expect("sent");
    let mut snapshot = machine_snapshot(&build(), "build");
    snapshot.boot_id = "remote-boot-2".into();
    state.set_endpoint_snapshot(&build(), Box::new(snapshot));
    let mut outcome = ClientShellInput::default();
    state.tick_machines(Instant::now() + MINUTE, &mut outcome);
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);

    // Its new list came first, with a card of the same id.
    let mut state = deferred_dismissal();
    dismissal(&mut state, Duration::ZERO).expect("sent");
    state.receive_agent_notices(
        &build(),
        AgentNoticesPayload {
            boot_id: "remote-boot-2".into(),
            revision: 1,
            notices: vec![card("n1")],
            initial: true,
        },
    );
    let mut snapshot = machine_snapshot(&build(), "build");
    snapshot.boot_id = "remote-boot-2".into();
    state.set_endpoint_snapshot(&build(), Box::new(snapshot));
    let mut outcome = ClientShellInput::default();
    state.tick_machines(Instant::now() + MINUTE, &mut outcome);
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
    assert!(shown_cards(&state).contains(&(build(), "n1")));
}

#[test]
fn the_fold_line_dismisses_every_machines_cards() {
    let mut state = two_cards();
    let mut outcome = ClientShellInput::default();
    state.dismiss_all_agent_cards(&mut outcome);
    assert_eq!(sent_to(&outcome, &LOCAL), ["agent.notice_dismiss"]);
    assert!(shown_cards(&state).is_empty());
    assert!(tick_machines(&mut state).actions.is_empty());
    switch(&mut state, &build());
    assert_eq!(
        sent_to(&tick_machines(&mut state), &build()),
        ["agent.notice_dismiss"]
    );
}

#[test]
fn a_card_of_a_machine_that_is_away_can_be_dismissed_and_goes_with_it() {
    let mut state = two_cards();
    push_cards(&mut state, &build(), 3, &["n1", "n2"]);
    state.mark_endpoint_disconnected(&build());
    for _ in 0..2 {
        state.visible_endpoint_notice = None;
        let outcome = click_card(&mut state, &build());
        assert!(outcome.actions.is_empty());
        assert_eq!(
            state
                .visible_endpoint_notice
                .as_ref()
                .map(|n| n.body.as_str()),
            Some("Build is unavailable")
        );
    }
    let mut outcome = ClientShellInput::default();
    state.dismiss_agent_card(&build(), "n1".into(), &mut outcome);
    assert!(outcome.actions.is_empty());
    assert_eq!(shown_cards(&state), [(build(), "n2"), (LOCAL, "n1")]);
    // Back, and shown: the dismissal is sent.
    state.set_endpoint_status(&build(), ClientEndpointStatus::Online);
    switch(&mut state, &build());
    assert_eq!(
        sent_to(&tick_machines(&mut state), &build()),
        ["agent.notice_dismiss"]
    );
    switch(&mut state, &LOCAL);
    // Turned off: gone, though it stays in the list.
    state.retire_endpoint(&build());
    assert_eq!(shown_cards(&state), [(LOCAL, "n1")]);
    state.set_endpoint_catalog(&[]);
    assert_eq!(shown_cards(&state), [(LOCAL, "n1")]);
}

// -------------------------------------------------------------- reminders

fn important(state: &mut ClientShellState, endpoint_id: &ClientEndpointId, name: &str) {
    let mut snapshot = machine_snapshot(endpoint_id, name);
    snapshot.revision = 2;
    snapshot.tabs[1].important = true;
    state.set_endpoint_snapshot(endpoint_id, Box::new(snapshot));
}

fn remind_after(state: &mut ClientShellState, start: Instant, minutes: u32) -> Vec<String> {
    state.visible_notifications.clear();
    state.tick_notifications(start + MINUTE * minutes);
    titles(state).into_iter().map(str::to_owned).collect()
}

fn news_info(tab_id: Option<&str>) -> NewsGetInfo {
    NewsGetInfo {
        enabled: true,
        times: Vec::new(),
        quiet_hours: String::new(),
        model: None,
        tab_id: tab_id.map(str::to_owned),
        pane_id: None,
        next_run_at: None,
        run: None,
        last_run: None,
        unread: false,
        consecutive_failures: 0,
        pending_notifications: 0,
        last_read_edition: None,
        new_stories: None,
    }
}

fn pull_news(state: &mut ClientShellState, tab_id: Option<&str>) {
    let mut outcome = ClientShellInput::default();
    state.pull_news_now(&mut outcome);
    let requests = sent(&outcome.actions);
    let [(to, id, Method::NewsGet(_))] = &requests[..] else {
        panic!("{requests:?}");
    };
    assert_eq!(to, &state.active_endpoint_id);
    answer(
        state,
        id,
        ResponseResult::NewsGet {
            news: news_info(tab_id),
        },
    );
}

#[test]
fn the_news_tab_of_a_machine_not_shown_raises_no_reminder() {
    for (asked, news_label) in [(true, "my news"), (false, "News")] {
        let mut state = two_machines();
        if asked {
            // Local named its News tab, then Build is shown.
            pull_news(&mut state, Some("tab_2"));
        }
        important(&mut state, &LOCAL, "local");
        let mut snapshot = state.endpoints[0].snapshot.clone().unwrap();
        snapshot.revision = 3;
        snapshot.tabs[1].label = news_label.into();
        state.set_endpoint_snapshot(&LOCAL, snapshot);
        important(&mut state, &build(), "build");
        switch(&mut state, &build());
        assert!(
            state.news.info.is_none(),
            "the shown machine has not answered"
        );
        let start = Instant::now();
        state.tick_notifications(start);
        // `tab_2` is News on Local and an agent's tab on Build.
        assert_eq!(
            remind_after(&mut state, start, 6),
            ["Build · build-2 still waiting"],
            "asked: {asked}"
        );
    }
    // Local named its desk: another tab with that label is the user's own.
    let mut state = two_machines();
    pull_news(&mut state, Some("tab_3"));
    important(&mut state, &LOCAL, "local");
    let mut snapshot = state.endpoints[0].snapshot.clone().unwrap();
    snapshot.revision = 3;
    snapshot.tabs[1].label = "News".into();
    state.set_endpoint_snapshot(&LOCAL, snapshot);
    switch(&mut state, &build());
    let start = Instant::now();
    state.tick_notifications(start);
    assert_eq!(
        remind_after(&mut state, start, 6),
        ["Local · News still waiting"]
    );
    // Shown again, its last answer gone (the `spaces` layout keeps none) and
    // the next not there yet: still the one it named.
    switch(&mut state, &LOCAL);
    let mut snapshot = state.endpoints[0].snapshot.clone().unwrap();
    snapshot.revision = 4;
    for tab in &mut snapshot.tabs {
        tab.important = tab.tab_id == "tab_3";
        if tab.important {
            tab.agent_status = AgentStatus::Blocked;
        }
    }
    snapshot.agents[0].tab_id = "tab_3".into();
    state.set_endpoint_snapshot(&LOCAL, snapshot);
    state.news.info = None;
    let start = start + MINUTE * 10;
    state.tick_notifications(start);
    assert!(remind_after(&mut state, start, 6).is_empty());
    // The precondition: an ordinary tab of a machine not shown does remind.
    let mut state = two_machines();
    important(&mut state, &LOCAL, "local");
    switch(&mut state, &build());
    let start = Instant::now();
    state.tick_notifications(start);
    assert_eq!(
        remind_after(&mut state, start, 6),
        ["Local · local-2 still waiting"]
    );
}

#[test]
fn the_coordinators_tab_of_a_machine_not_shown_raises_no_reminder() {
    let mut state = two_machines();
    state.handle_coordinator_endpoint_result(
        CoordinatorRequestKind::Get,
        Ok(ResponseResult::CoordinatorGet {
            info: CoordinatorGetInfo {
                enabled: true,
                state: CoordinatorStateInfo::Running,
                tab_id: Some("tab_2".into()),
                ..CoordinatorGetInfo::default()
            },
        }),
    );
    important(&mut state, &LOCAL, "local");
    switch(&mut state, &build());
    let start = Instant::now();
    state.tick_notifications(start);
    assert!(remind_after(&mut state, start, 6).is_empty());
    // Local restarted: the ids are another server's, the name is forgotten.
    let mut snapshot = machine_snapshot(&LOCAL, "local");
    snapshot.boot_id = "boot-2".into();
    snapshot.tabs[1].important = true;
    state.set_endpoint_snapshot(&LOCAL, Box::new(snapshot));
    let start = start + MINUTE * 10;
    state.tick_notifications(start);
    assert_eq!(
        remind_after(&mut state, start, 6),
        ["Local · local-2 still waiting"]
    );
}

#[test]
fn mutes_and_clocks_are_kept_per_machine() {
    let mut state = two_machines();
    state.receive_tab_mutes(
        &LOCAL,
        TabMutesPayload {
            boot_id: LOCAL_BOOT.into(),
            revision: 1,
            tab_ids: vec!["tab_2".into()],
        },
    );
    important(&mut state, &LOCAL, "local");
    important(&mut state, &build(), "build");
    let (_, _) = state.receive_notification(&LOCAL, attention(2), Instant::now());
    assert!(titles(&state).is_empty(), "muted on Local");
    toast(&mut state, &build());
    let start = Instant::now();
    state.tick_notifications(start);
    assert_eq!(
        remind_after(&mut state, start, 6),
        ["Build · build-2 still waiting"]
    );
    assert!(state
        .idle_reminders
        .contains_key(&(build(), "tab_2".to_owned())));
    assert!(!state
        .idle_reminders
        .contains_key(&(LOCAL, "tab_2".to_owned())));
}

#[test]
fn a_machine_that_is_away_reminds_of_nothing_until_it_is_back() {
    let mut state = two_machines();
    let mut snapshot = machine_snapshot(&build(), "build");
    snapshot.revision = 2;
    snapshot.tabs[1].important = true;
    snapshot.tabs[2].remind_every = Some(crate::api::schema::TabRemindInterval::M10);
    state.set_endpoint_snapshot(&build(), Box::new(snapshot));
    let start = Instant::now();
    state.tick_notifications(start);
    state.mark_endpoint_disconnected(&build());
    for minutes in [6, 11, 22] {
        assert!(remind_after(&mut state, start, minutes).is_empty());
        let due = state.next_idle_reminder_deadline().expect("still tracked");
        assert!(due > start + MINUTE * minutes, "the loop does not spin");
    }
    state.set_endpoint_status(&build(), ClientEndpointStatus::Online);
    assert!(
        remind_after(&mut state, start, 23).is_empty(),
        "no burst on the return"
    );
    let mut titles = remind_after(&mut state, start, 33);
    titles.sort();
    assert_eq!(
        titles,
        ["Build · build-2 still waiting", "Build · build-3 reminder"]
    );

    // One machine keeps its clocks while its server is away.
    let mut state = one_machine();
    important(&mut state, &LOCAL, "local");
    let start = Instant::now();
    state.tick_notifications(start);
    state.mark_endpoint_disconnected(&LOCAL);
    assert_eq!(
        remind_after(&mut state, start, 6),
        ["local-2 still waiting"]
    );
}

// ----------------------------------------------- one machine, one disabled

/// Local and a saved machine that is turned off: one machine.
fn local_and_a_disabled_machine() -> ClientShellState {
    let mut state = one_machine();
    let mut off = profile();
    off.enabled = false;
    state.set_endpoint_catalog(&[off]);
    assert!(state.multi_endpoint_active());
    state.compose(COLS, ROWS).unwrap();
    state
}

#[test]
fn a_disabled_machine_brings_no_names() {
    let mut state = local_and_a_disabled_machine();
    toast(&mut state, &LOCAL);
    assert_eq!(titles(&state), ["local-2 needs attention"]);
    push_cards(&mut state, &LOCAL, 1, &[]);
    push_cards(&mut state, &LOCAL, 2, &["n1"]);
    assert!(state.agent_card_views()[0].machine.is_none());
}

#[test]
fn a_disabled_machine_holds_no_reminders_of_local() {
    let mut state = local_and_a_disabled_machine();
    important(&mut state, &LOCAL, "local");
    let start = Instant::now();
    state.tick_notifications(start);
    state.mark_endpoint_disconnected(&LOCAL);
    assert_eq!(
        remind_after(&mut state, start, 6),
        ["local-2 still waiting"]
    );
}

#[test]
fn a_disabled_machine_dims_nothing_of_local() {
    let mut state = local_and_a_disabled_machine();
    let sidebar = |state: &mut ClientShellState| {
        let frame = state.compose(COLS, ROWS).expect("frame").frame;
        let width = usize::from(state.layout(COLS, ROWS).sidebar.width);
        frame
            .cells
            .chunks(usize::from(frame.width))
            .flat_map(|row| row[..width].to_vec())
            .collect::<Vec<_>>()
    };
    let online = sidebar(&mut state);
    state.set_endpoint_status(&LOCAL, ClientEndpointStatus::Reconnecting);
    assert!(sidebar(&mut state) == online, "drawn as with one machine");
}

#[test]
fn a_disabled_machine_gives_the_settings_no_scope() {
    let mut state = local_and_a_disabled_machine();
    state.open_settings_overlay();
    let text = frame_text(&mut state);
    assert!(text.contains(" settings "), "{text}");
    assert!(!text.contains("settings ·"), "{text}");
}

// ------------------------------------------------------------------ notes

/// A tab without an agent: its notes key is its tab id's, so both machines'
/// `tab_1` have this one.
const NOTES_KEY: &str = "tab-tab_1";

fn notes(text: &str) -> NotesInfo {
    NotesInfo {
        key: NOTES_KEY.into(),
        path: format!("/tmp/notes/{NOTES_KEY}.md"),
        revision: format!("rev-{text}"),
        exists: true,
        unchanged: false,
        text: Some(text.into()),
        bytes: text.len() as u64,
        updated_at: None,
        updated_by: None,
        agent: None,
        session_id: None,
        tab_id: Some("tab_1".into()),
        pane_id: Some("pane_1".into()),
        previous: None,
    }
}

fn tick_notes(state: &mut ClientShellState, now: Instant) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_info_dock(now, &mut outcome);
    outcome
}

/// The shown machine's dock open on `tab_1` with `text` loaded.
fn open_notes(state: &mut ClientShellState, text: &str) {
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane_for_focused_tab(&mut outcome);
    load_notes(state, outcome, text);
    state.set_pane_surface(boot_surface(&state.active_endpoint_id.clone()));
    state.compose(COLS, ROWS).unwrap();
}

fn load_notes(state: &mut ClientShellState, outcome: ClientShellInput, text: &str) {
    let requests = sent(&outcome.actions);
    let [(to, id, Method::NotesGet(_))] = &requests[..] else {
        panic!("{requests:?}");
    };
    assert_eq!(to, &state.active_endpoint_id);
    answer(state, id, ResponseResult::NotesGet { notes: notes(text) });
}

/// Type into the shown machine's notes.
fn type_notes(state: &mut ClientShellState, text: &str) {
    state.info_dock.as_mut().unwrap().focused = true;
    let key = |code| {
        RawInputEvent::Key(crate::input::TerminalKey::new(
            code,
            crossterm::event::KeyModifiers::empty(),
        ))
    };
    if !state.info_dock.as_ref().unwrap().editing() {
        state.handle_raw_events(vec![key(KeyCode::Char('e'))]);
    }
    for ch in text.chars() {
        state.handle_raw_events(vec![key(KeyCode::Char(ch))]);
    }
}

fn saves(outcome: &ClientShellInput) -> Vec<(ClientEndpointId, String, Option<String>)> {
    sent(&outcome.actions)
        .into_iter()
        .filter_map(|(to, _, method)| match method {
            Method::NotesSet(params) => Some((to, params.text, params.base_revision)),
            _ => None,
        })
        .collect()
}

#[test]
fn text_typed_for_one_machine_is_never_written_to_another() {
    let mut state = two_machines();
    open_notes(&mut state, "local page");
    type_notes(&mut state, "!");
    assert!(state.info_dock_editing());

    switch(&mut state, &build());
    let dock = state.info_dock.as_ref().unwrap();
    assert!(
        dock.editor.is_none() && !dock.focused,
        "Local's buffer is put away"
    );
    let late = Instant::now() + MINUTE;
    let outcome = tick_notes(&mut state, late);
    assert_eq!(saves(&outcome), []);
    let notice = state.visible_endpoint_notice.as_ref().expect("said once");
    assert!(notice.body.contains("Local"), "{}", notice.body);
    // Build's own page of the same key, edited and saved meanwhile.
    open_notes(&mut state, "build page");
    type_notes(&mut state, "?");
    let outcome = tick_notes(&mut state, late);
    assert_eq!(
        saves(&outcome),
        [(
            build(),
            "build page?".to_owned(),
            Some("rev-build page".to_owned())
        )]
    );
    let dock = state.info_dock.as_mut().unwrap();
    dock.editor = None;
    dock.data.in_flight = None;
    state.pending_requests.clear();

    // Back on Local it is saved there, against the page it was typed on.
    switch(&mut state, &LOCAL);
    let outcome = tick_notes(&mut state, late);
    assert_eq!(
        saves(&outcome),
        [(
            LOCAL,
            "local page!".to_owned(),
            Some("rev-local page".to_owned())
        )]
    );
    let request = sent(&outcome.actions)[0].1.clone();
    answer(
        &mut state,
        &request,
        ResponseResult::NotesWrite {
            write: crate::api::schema::notes::NotesWriteInfo {
                outcome: crate::api::schema::notes::NotesWriteOutcome::Written,
                notes: notes("local page!"),
            },
        },
    );
    assert!(state.info_dock.as_ref().unwrap().editor.is_none(), "saved");
}

#[test]
fn a_queued_tick_waits_for_its_machine() {
    let mut state = two_machines();
    open_notes(&mut state, "- [ ] one");
    // Local is busy with a command, so what is queued stays queued.
    let mut busy = ClientShellInput::default();
    state.push_endpoint_method(Method::NewsRun(Default::default()), &mut busy);
    state.info_dock.as_mut().unwrap().queue(
        InfoRequest::NotesSet {
            key: NOTES_KEY.into(),
            text: "- [x] one".into(),
            base_revision: Some("rev".into()),
            tick: true,
        },
        None,
    );
    assert!(saves(&tick_notes(&mut state, Instant::now())).is_empty());

    switch(&mut state, &build());
    let late = Instant::now() + MINUTE;
    assert_eq!(saves(&tick_notes(&mut state, late)), []);
    assert!(state.info_dock.as_ref().unwrap().data.queue.is_empty());
    switch(&mut state, &LOCAL);
    assert_eq!(
        saves(&tick_notes(&mut state, late)),
        [(LOCAL, "- [x] one".to_owned(), Some("rev".to_owned()))]
    );
}

#[test]
fn a_dock_shows_its_own_machines_page() {
    let mut state = two_machines();
    open_notes(&mut state, "local page");
    switch(&mut state, &build());
    open_notes(&mut state, "build page");
    switch(&mut state, &LOCAL);
    let text = frame_text(&mut state);
    assert!(!text.contains("build page"), "{text}");
    let outcome = tick_notes(&mut state, Instant::now());
    load_notes(&mut state, outcome, "local page");
    assert!(frame_text(&mut state).contains("local page"));
    // Nothing was typed: nothing is said.
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn a_removed_machines_unsaved_notes_are_dropped_and_said() {
    let mut state = on_build();
    open_notes(&mut state, "build page");
    type_notes(&mut state, "!");
    state.retire_endpoint(&build());
    state.select_unavailable_local();
    state.set_endpoint_catalog(&[]);
    switch(&mut state, &LOCAL);
    let late = Instant::now() + MINUTE;
    assert_eq!(saves(&tick_notes(&mut state, late)), []);
    let notice = state.visible_endpoint_notice.as_ref().expect("said");
    assert!(notice.body.contains("removed"), "{}", notice.body);
    assert!(state.info_dock.as_ref().unwrap().parked.is_empty());
}

#[test]
fn a_machine_removed_later_takes_its_parked_notes_along() {
    let mut state = on_build();
    open_notes(&mut state, "build page");
    type_notes(&mut state, "!");
    switch(&mut state, &LOCAL);
    tick_notes(&mut state, Instant::now());
    state.visible_endpoint_notice = None;
    // Turned off keeps them, removed drops them.
    state.retire_endpoint(&build());
    tick_notes(&mut state, Instant::now());
    assert!(state.visible_endpoint_notice.is_none());
    assert_eq!(state.info_dock.as_ref().unwrap().parked.len(), 1);
    state.set_endpoint_catalog(&[]);
    tick_notes(&mut state, Instant::now());
    let notice = state.visible_endpoint_notice.as_ref().expect("said");
    assert!(notice.body.contains("removed"), "{}", notice.body);
    assert!(state.info_dock.as_ref().unwrap().parked.is_empty());
}

#[test]
fn every_removed_machine_says_that_its_notes_went() {
    let mut other = profile();
    other.id = ProfileId::parse("fedcba9876543210fedcba9876543210").unwrap();
    other.label = "Other".into();
    let other_id = ClientEndpointId::Ssh(other.id.clone());
    let mut state = two_machines();
    state.set_endpoint_catalog(&[profile(), other.clone()]);
    state.set_endpoint_status(&other_id, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot(&other_id, Box::new(machine_snapshot(&other_id, "other")));
    for endpoint_id in [build(), other_id] {
        switch(&mut state, &endpoint_id);
        open_notes(&mut state, "a page");
        type_notes(&mut state, "!");
    }
    switch(&mut state, &LOCAL);
    tick_notes(&mut state, Instant::now());
    for catalog in [&[other][..], &[]] {
        state.visible_endpoint_notice = None;
        state.set_endpoint_catalog(catalog);
        tick_notes(&mut state, Instant::now());
        let notice = state.visible_endpoint_notice.as_ref().expect("said");
        assert!(notice.body.contains("removed"), "{}", notice.body);
    }
}

#[test]
fn notes_that_are_back_before_the_tick_are_not_said_to_wait() {
    let mut state = two_machines();
    open_notes(&mut state, "local page");
    type_notes(&mut state, "!");
    switch(&mut state, &build());
    switch(&mut state, &LOCAL);
    // Something is parked, so the tick has a look.
    let dock = state.info_dock.as_ref().unwrap();
    assert!(dock.parked.contains_key(&build()));
    tick_notes(&mut state, Instant::now());
    assert!(state.visible_endpoint_notice.is_none());
}

/// The user comes back to a tab whose dock is closed (a toast, a card).
#[test]
fn a_restored_tick_is_sent_while_the_dock_is_closed() {
    let mut state = two_machines();
    open_notes(&mut state, "- [ ] one");
    let mut busy = ClientShellInput::default();
    state.push_endpoint_method(Method::NewsRun(Default::default()), &mut busy);
    let request = |tick_text: &str| InfoRequest::NotesSet {
        key: NOTES_KEY.into(),
        text: tick_text.into(),
        base_revision: Some("rev".into()),
        tick: true,
    };
    state
        .info_dock
        .as_mut()
        .unwrap()
        .queue(request("- [x] one"), None);
    assert!(saves(&tick_notes(&mut state, Instant::now())).is_empty());
    switch(&mut state, &build());
    state.info_dock.as_mut().unwrap().open_tabs.clear();
    switch(&mut state, &LOCAL);
    let now = Instant::now() + MINUTE;
    let retry = now + Duration::from_millis(50);
    let dock = state.info_dock.as_mut().unwrap();
    assert!(dock.editor.is_none() && dock.flash.is_none());
    dock.queue(request("- [x] one"), Some(retry));
    assert_eq!(state.next_info_dock_deadline(now), Some(retry));
    assert_eq!(
        saves(&tick_notes(&mut state, retry)),
        [(LOCAL, "- [x] one".to_owned(), Some("rev".to_owned()))]
    );
    // What only the open dock needs is no reason to wake.
    let mut state = two_machines();
    open_notes(&mut state, "page");
    let dock = state.info_dock.as_mut().unwrap();
    dock.open_tabs.clear();
    dock.queue(InfoRequest::NotesGet, Some(retry));
    assert_eq!(state.next_info_dock_deadline(now), None);
    assert!(tick_notes(&mut state, retry).actions.is_empty());
}

#[test]
fn a_lost_machine_keeps_its_unsaved_notes_until_it_is_back() {
    let mut state = on_build();
    open_notes(&mut state, "build page");
    type_notes(&mut state, "!");
    state.mark_endpoint_disconnected(&build());
    let late = Instant::now() + MINUTE;
    assert_eq!(saves(&tick_notes(&mut state, late)), []);
    state.set_endpoint_status(&build(), ClientEndpointStatus::Online);
    assert_eq!(
        saves(&tick_notes(&mut state, late + MINUTE)),
        [(
            build(),
            "build page!".to_owned(),
            Some("rev-build page".to_owned())
        )]
    );
}

/// The runtime cancels a machine's requests when it is left; what was queued
/// behind one must not go out from inside the cancellation (it would be
/// dropped, with its reply waited for).
#[test]
fn a_cancelled_dock_request_starts_no_other() {
    let mut state = two_machines();
    open_notes(&mut state, "local page");
    type_notes(&mut state, "!");
    let dock = state.info_dock.as_mut().unwrap();
    dock.queue(InfoRequest::NotesGet, None);
    let mut outcome = ClientShellInput::default();
    state.pump_info_dock(Instant::now(), &mut outcome);
    let request = sent(&outcome.actions)[0].1.clone();
    assert_eq!(saves(&tick_notes(&mut state, Instant::now() + MINUTE)), []);
    assert_eq!(state.info_dock.as_ref().unwrap().data.queue.len(), 1);
    state.cancel_endpoint_request(&request);
    assert!(state.info_dock.as_ref().unwrap().data.in_flight.is_none());
    assert_eq!(
        saves(&tick_notes(&mut state, Instant::now() + MINUTE)).len(),
        1,
        "the tick goes on"
    );
}

#[test]
fn another_machines_notes_reveal_leaves_the_keyboard_alone() {
    let mut state = two_machines();
    open_notes(&mut state, "local page");
    type_notes(&mut state, "!");
    let outcome = state.receive_notes_reveal(
        &build(),
        NotesRevealPayload {
            boot_id: REMOTE_BOOT.into(),
            revision: 1,
            tab_id: "tab_3".into(),
        },
    );
    assert!(outcome.actions.is_empty() && !outcome.resize);
    assert!(state.info_dock_editing(), "still typing in Local's notes");
    let dock = state.info_dock.as_ref().unwrap();
    assert!(dock.open_tabs.contains(&(build(), "tab_3".to_owned())));
    assert!(!dock.open_tabs.contains(&(LOCAL, "tab_3".to_owned())));
}

// --------------------------------------------- news, coordinator, teams

fn coordinator_info() -> CoordinatorGetInfo {
    CoordinatorGetInfo {
        enabled: true,
        state: CoordinatorStateInfo::Running,
        tab_id: Some("tab_3".into()),
        dashboard_url: Some("http://127.0.0.1:7718/".into()),
        ..CoordinatorGetInfo::default()
    }
}

#[test]
fn coordinator_and_news_actions_go_to_the_selected_machine() {
    let mut state = on_build();
    state.coordinator.info = Some(coordinator_info());
    pull_news(&mut state, Some("tab_2"));
    use crate::client::shell::coordinator::CoordinatorMenuAction as Menu;
    for (action, expected) in [
        (Menu::WakeNow, "coordinator.wake"),
        (Menu::Restart, "coordinator.start"),
        (Menu::ToggleEnabled, "coordinator.set_enabled"),
        (Menu::Focus, "tab.focus"),
    ] {
        let mut outcome = ClientShellInput::default();
        state.activate_coordinator_context_action(
            ClientContextMenuAction::Coordinator(action),
            &mut outcome,
        );
        assert_eq!(sent_to(&outcome, &build()), [expected]);
    }
    for (action, expected) in [
        (ClientContextMenuAction::NewsRun, "news.run"),
        (ClientContextMenuAction::NewsOpen, "news.open"),
        (
            ClientContextMenuAction::NewsToggleSchedule,
            "news.set_enabled",
        ),
    ] {
        let mut outcome = ClientShellInput::default();
        state.activate_news_context_action(true, action, &mut outcome);
        assert_eq!(sent_to(&outcome, &build()), [expected]);
    }
    // Local's rows are not Build's.
    switch(&mut state, &LOCAL);
    assert!(state.coordinator_row().is_none() && state.news_row().is_none());
}

#[test]
fn a_remote_dashboard_is_never_opened_on_this_machine() {
    use crate::client::shell::coordinator::CoordinatorMenuAction as Menu;
    for (endpoint_id, opens) in [(build(), false), (LOCAL, true)] {
        let mut state = two_machines();
        switch(&mut state, &endpoint_id);
        state.coordinator.info = Some(coordinator_info());
        let mut outcome = ClientShellInput::default();
        state.activate_coordinator_context_action(
            ClientContextMenuAction::Coordinator(Menu::OpenDashboard),
            &mut outcome,
        );
        let requests = sent(&outcome.actions);
        let [(to, id, Method::CoordinatorOpenDashboard(params))] = &requests[..] else {
            panic!("{requests:?}");
        };
        assert_eq!((to, params.open), (&endpoint_id, opens));
        answer(
            &mut state,
            id,
            ResponseResult::CoordinatorGet {
                info: coordinator_info(),
            },
        );
        let said = state.endpoint_error.take();
        assert_eq!(said.is_some(), !opens, "{said:?}");
        if let Some(said) = said {
            assert!(said.contains("127.0.0.1:7718"), "{said}");
            assert!(!said.contains("http"), "no link to click: {said}");
        }
        // A click on its link in a pane of that machine.
        for (url, dashboard) in [
            ("http://localhost:7718/", true),
            ("http://127.0.0.1:7718/?a", true),
            ("http://127.0.0.1:3000/", false),
            ("https://example.test:7718/", false),
        ] {
            let hit = state.hits.panes[0].clone();
            let mut outcome = ClientShellInput::default();
            state.push_endpoint_method_with_kind(
                Method::PaneLinkActivate(crate::api::schema::PaneLinkActivateParams {
                    pane_id: hit.pane_id.clone(),
                    viewport_row: 0,
                    col: 0,
                    content_revision: None,
                    offset_from_bottom: None,
                }),
                PendingEndpointKind::PaneLinkActivate {
                    pane_id: hit.pane_id,
                    inner_rect: hit.inner_rect,
                    fallback_events: Vec::new(),
                },
                &mut outcome,
            );
            let id = sent(&outcome.actions)[0].1.clone();
            let (_, actions) = answer(
                &mut state,
                &id,
                ResponseResult::PaneLinkActivated {
                    url: Some(url.into()),
                    handled: false,
                },
            );
            let opened = matches!(&actions[..], [ClientShellAction::OpenSafeWebUrl(_)]);
            assert_eq!(opened, opens || !dashboard, "{url} on {endpoint_id:?}");
            assert_eq!(state.endpoint_error.take().is_some(), !opened);
        }
    }
}

#[test]
fn a_remote_dashboard_is_no_link_in_the_settings() {
    for (endpoint_id, link) in [(build(), false), (LOCAL, true)] {
        let mut state = two_machines();
        switch(&mut state, &endpoint_id);
        state.coordinator.info = Some(coordinator_info());
        state.open_settings_overlay();
        let mut outcome = ClientShellInput::default();
        state.select_settings_section(ClientSettingsSection::Coordinator, &mut outcome);
        let text = frame_text(&mut state);
        assert!(text.contains("127.0.0.1:7718"), "{text}");
        assert_eq!(text.contains("http://127.0.0.1:7718"), link, "{text}");
    }
}

#[test]
fn overlays_menus_and_the_force_button_end_with_the_switch() {
    let team_request = |state: &mut ClientShellState| {
        let mut outcome = ClientShellInput::default();
        state.push_team_request(
            super::super::teams::TeamRequest::Leave {
                pane_id: "pane_2".into(),
            },
            &mut outcome,
        );
        outcome
    };
    let mut state = on_build();
    assert_eq!(sent_to(&team_request(&mut state), &build()), ["team.leave"]);
    for open in [0, 1, 2, 3] {
        switch(&mut state, &build());
        match open {
            0 => state.open_team_overlay("ws_1".into()),
            1 => state.open_settings_overlay(),
            2 => state.open_tab_context_menu("tab_2".into(), 3, 3),
            _ => state.open_team_purpose_rename("ws_1".into(), "x", false, false),
        }
        assert!(state.overlay.is_some());
        switch(&mut state, &LOCAL);
        assert!(state.overlay.is_none(), "{open}");
    }
    // A refused suspend on Build offers to force it, until Build is left.
    switch(&mut state, &build());
    let mut outcome = ClientShellInput::default();
    state.push_endpoint_method(
        Method::AgentSuspend(crate::api::schema::AgentSuspendParams {
            target: "pane_2".into(),
        }),
        &mut outcome,
    );
    let request = sent(&outcome.actions)[0].1.clone();
    state.handle_endpoint_result(
        REMOTE_BOOT,
        &request,
        Err(ClientShellEndpointError {
            code: Some("agent_working".into()),
            message: "working".into(),
        }),
    );
    let force = state
        .visible_endpoint_notice
        .as_ref()
        .and_then(|notice| notice.force.as_ref());
    assert!(force.is_some());
    switch(&mut state, &LOCAL);
    assert!(state.visible_endpoint_notice.is_none());
}

/// A reply of the machine that was left never lands on the one shown, though
/// the pane it names exists there too.
#[test]
fn a_late_reply_of_the_machine_left_is_dropped() {
    for left in [true, false] {
        let mut state = on_build();
        let mut outcome = ClientShellInput::default();
        state.pull_news_now(&mut outcome);
        let request = sent(&outcome.actions)[0].1.clone();
        if left {
            switch(&mut state, &LOCAL);
        }
        let (repaint, actions) = state.handle_endpoint_result(
            REMOTE_BOOT,
            &request,
            Ok(ResponseResult::NewsGet {
                news: news_info(Some("tab_2")),
            }),
        );
        assert_eq!(repaint, !left);
        assert!(actions.is_empty());
        assert_eq!(state.news.info.is_none(), left);
    }
}

// --------------------------------------------------------------- settings

#[test]
fn settings_say_whose_they_are_with_several_machines() {
    let mut state = on_build();
    for (section, scope) in [
        (
            ClientSettingsSection::Theme,
            "this client, for every machine",
        ),
        (
            ClientSettingsSection::Indicators,
            "this client, for every machine",
        ),
        (
            ClientSettingsSection::Sound,
            "this client, for every machine",
        ),
        (
            ClientSettingsSection::Toast,
            "this client, for every machine",
        ),
        (
            ClientSettingsSection::Reminders,
            "this client, for every machine",
        ),
        (ClientSettingsSection::Integrations, "on Build"),
        (ClientSettingsSection::Backups, "on Build"),
        (ClientSettingsSection::ClosedSessions, "on Build"),
        (ClientSettingsSection::News, "on Build"),
        (ClientSettingsSection::Browser, "on Build"),
        (ClientSettingsSection::Coordinator, "on Build"),
        (ClientSettingsSection::Agents, "on Build"),
    ] {
        state.open_settings_overlay();
        let mut outcome = ClientShellInput::default();
        state.select_settings_section(section, &mut outcome);
        for (to, _, method) in sent(&outcome.actions) {
            assert_eq!(to, build(), "{method:?}");
        }
        let text = frame_text(&mut state);
        assert!(text.contains(&format!("settings · {scope}")), "{text}");
    }
    let mut state = one_machine();
    state.open_settings_overlay();
    let text = frame_text(&mut state);
    assert!(text.contains(" settings "), "{text}");
    assert!(!text.contains("settings ·"), "{text}");
}

#[test]
fn quiet_hours_of_an_older_machine_are_not_written_here() {
    let dir = std::env::temp_dir().join(format!("herdr-quiet-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\n").unwrap();
    // nextest runs each test in its own process
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
    let mut state = on_build();
    state.set_endpoint_methods_for(
        &build(),
        Some(vec!["news.get".into(), "server.reload_config".into()]),
    );
    pull_news(&mut state, Some("tab_2"));
    state.open_settings_overlay();
    let mut outcome = ClientShellInput::default();
    state.select_settings_section(ClientSettingsSection::News, &mut outcome);
    let Some(ClientShellOverlay::Settings(settings)) = state.overlay.as_mut() else {
        panic!("settings");
    };
    settings.news.info = Some(news_info(Some("tab_2")));
    settings.news.picker = Some(super::super::settings_news::ClientNewsPicker::QuietHours(
        vec!["22:00-08:00".into()],
    ));
    let mut outcome = ClientShellInput::default();
    state.apply_news_choice(0, &mut outcome);
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
    let notice = state.visible_endpoint_notice.as_ref().expect("said");
    assert!(
        notice.body.contains("news.set_quiet_hours"),
        "{}",
        notice.body
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[ui]\n");
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn saving_a_client_setting_reloads_locals_server_not_the_selected_machine() {
    let dir = std::env::temp_dir().join(format!("herdr-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\n").unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
    let mut state = on_build();
    let save = |state: &mut ClientShellState| {
        let mut outcome = ClientShellInput::default();
        assert!(state.save_settings_edit(
            crate::config::ConfigEdit::IdleReminderMinutes(15),
            &mut outcome
        ));
        outcome
    };
    let outcome = save(&mut state);
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
    assert!(std::fs::read_to_string(&path).unwrap().contains("15"));
    assert!(tick_machines(&mut state).actions.is_empty());
    switch(&mut state, &LOCAL);
    assert_eq!(
        sent_to(&tick_machines(&mut state), &LOCAL),
        ["server.reload_config"]
    );
    assert!(tick_machines(&mut state).actions.is_empty(), "once");
    // On Local at once, as before.
    assert_eq!(sent_to(&save(&mut state), &LOCAL), ["server.reload_config"]);
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(dir);
}

// ------------------------------------------------------------------ menus

fn menu_labels(state: &mut ClientShellState) -> Vec<String> {
    state.open_tab_context_menu("tab_2".into(), 3, 3);
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("menu");
    };
    menu.items()
        .iter()
        .map(|item| item.label.to_string())
        .collect()
}

/// Run the menu item `label` of `tab_2`; what was sent besides its focus.
fn run_item(state: &mut ClientShellState, label: &str) -> Vec<(ClientEndpointId, Method)> {
    let index = menu_labels(state)
        .iter()
        .position(|item| item == label)
        .unwrap_or_else(|| panic!("{label} is listed"));
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_mut() else {
        panic!("menu");
    };
    menu.highlighted = index;
    let mut outcome = ClientShellInput::default();
    state.handle_raw_events(Vec::new());
    state.activate_context_menu_item(index, &mut outcome);
    state.overlay = None;
    sent(&outcome.actions)
        .into_iter()
        .map(|(to, _, method)| (to, method))
        .filter(|(_, method)| !matches!(method, Method::TabFocus(_)))
        .collect()
}

#[test]
fn menu_actions_go_to_the_selected_machine() {
    let mut state = on_build();
    for (label, name) in [
        ("Suspend agent", "agent.suspend"),
        ("Restart agent", "agent.restart"),
        ("Pin", "tab.set_pinned"),
        ("Mute notifications", "tab.set_muted"),
        ("Do not disturb", "tab.set_dnd"),
        ("Important", "tab.set_reminder"),
    ] {
        let requests = run_item(&mut state, label);
        assert_eq!(requests.len(), 1, "{label}: {requests:?}");
        let (to, method) = &requests[0];
        assert_eq!(to, &build(), "{label}");
        assert_eq!(crate::api::api_method_name(method), name);
    }
    // Rename: the modal's save.
    run_item(&mut state, "Rename");
    state.open_tab_context_menu("tab_2".into(), 3, 3);
    state.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
        title: "rename tab",
        input: TextEditor::new("renamed", false),
        target: ClientRenameTarget::Tab {
            tab_id: "tab_2".into(),
            auto_name: false,
            original_name: "build-2".into(),
        },
    }));
    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Enter, crossterm::event::KeyModifiers::empty()),
    )]);
    assert_eq!(sent_to(&outcome, &build()), ["tab.rename"]);
}

#[test]
fn a_method_an_older_machine_lacks_disables_only_that_action() {
    let mut state = two_machines();
    let all = menu_labels(&mut state);
    state.overlay = None;
    for endpoint_id in [LOCAL, build()] {
        state.set_endpoint_methods_for(&endpoint_id, None);
    }
    state.set_endpoint_methods_for(
        &build(),
        Some(vec![
            "tab.focus".into(),
            "agent.suspend".into(),
            "tab.set_pinned".into(),
        ]),
    );
    switch(&mut state, &build());
    let labels = menu_labels(&mut state);
    assert!(
        all.contains(&"Mute notifications".to_owned())
            && !labels.contains(&"Mute notifications".to_owned())
    );
    assert!(labels.contains(&"Pin".to_owned()));
    state.overlay = None;
    assert_eq!(run_item(&mut state, "Suspend agent").len(), 1);
    assert!(state.visible_endpoint_notice.is_none());
    // Listed but not supported: said, nothing sent.
    assert!(run_item(&mut state, "Restart agent").is_empty());
    let notice = state.visible_endpoint_notice.as_ref().expect("said");
    assert!(notice.body.contains("agent.restart"), "{}", notice.body);
    // Local is not affected.
    switch(&mut state, &LOCAL);
    assert_eq!(run_item(&mut state, "Restart agent").len(), 1);
    assert_eq!(run_item(&mut state, "Mute notifications").len(), 1);
}

#[test]
fn an_action_on_the_selected_machine_while_it_is_away_is_answered_every_time() {
    let mut state = on_build();
    state.mark_endpoint_disconnected(&build());
    for attempt in 0..2 {
        state.visible_endpoint_notice = None;
        assert!(run_item(&mut state, "Suspend agent").is_empty());
        assert_eq!(
            state
                .visible_endpoint_notice
                .as_ref()
                .map(|notice| notice.body.as_str()),
            Some("Build is not ready"),
            "attempt {attempt}"
        );
    }
}
