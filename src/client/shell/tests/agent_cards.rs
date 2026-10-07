//! Agent cards (fork): the per-endpoint card list, its look, sound on new
//! ids only, clicks, dismissal and the surface-patch guard.

use super::*;
use crate::api::schema::{AgentNoticeInfo, AgentNoticeKind, Method};
use crate::client::endpoint::{ClientEndpointStatus, ProfileId, SavedSshEndpoint};
use crate::client::shell::agent_cards::{
    agent_cards_block_patch, body_lines, kind_accent, render_agent_card_banner, render_agent_cards,
    AgentCardHit, AgentCardView, CARD_WIDTH,
};
use crate::server::headless::agent_notices::AgentNoticesPayload;
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

const NOW: u64 = 1_800_000_000;

fn notice(id: &str, kind: AgentNoticeKind, pane_id: &str, tab_id: &str) -> AgentNoticeInfo {
    AgentNoticeInfo {
        id: id.into(),
        kind,
        title: format!("title {id}"),
        body: Some(format!("body {id}")),
        agent: Some("claude".into()),
        name: format!("agent-{id}"),
        pane_id: pane_id.into(),
        tab_id: Some(tab_id.into()),
        tab_label: Some(format!("tab {tab_id}")),
        workspace_id: Some("ws_1".into()),
        workspace_label: Some("client-shell".into()),
        unix: NOW - 120,
        team: None,
        role: None,
    }
}

fn default_palette() -> crate::app::state::Palette {
    crate::app::client_palette_from_config(&Config::default())
}

fn payload(revision: u64, initial: bool, notices: Vec<AgentNoticeInfo>) -> AgentNoticesPayload {
    AgentNoticesPayload {
        boot_id: "boot-1".into(),
        revision,
        notices,
        initial,
    }
}

fn state() -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Herdr;
    let mut state = ClientShellState::new(config);
    state.sidebar_collapsed = true;
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

fn sounds(effects: &[ClientShellNotificationEffect]) -> Vec<crate::sound::Sound> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            ClientShellNotificationEffect::Sound { sound, .. } => Some(*sound),
            _ => None,
        })
        .collect()
}

fn requests(outcome: &ClientShellInput) -> Vec<Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.method.clone()),
            _ => None,
        })
        .collect()
}

fn press(
    state: &mut ClientShellState,
    button: MouseButton,
    column: u16,
    row: u16,
) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(button),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn hit_rect(state: &ClientShellState, wanted: impl Fn(&AgentCardHit) -> bool) -> Rect {
    state
        .hits
        .agent_cards
        .iter()
        .find(|(_, hit)| wanted(hit))
        .map(|(rect, _)| *rect)
        .expect("agent card hit")
}

fn card_rect(state: &ClientShellState, wanted: &str) -> Rect {
    hit_rect(
        state,
        |hit| matches!(hit, AgentCardHit::Card { id, .. } if id == wanted),
    )
}

fn close_rect(state: &ClientShellState, wanted: &str) -> Rect {
    hit_rect(
        state,
        |hit| matches!(hit, AgentCardHit::Close { id, .. } if id == wanted),
    )
}

fn buffer_text(buffer: &Buffer) -> String {
    buffer
        .content
        .chunks(usize::from(buffer.area.width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn each_kind_draws_its_glyph_in_its_accent_on_the_panel_card() {
    let palette = default_palette();
    for kind in [
        AgentNoticeKind::Info,
        AgentNoticeKind::Question,
        AgentNoticeKind::Done,
        AgentNoticeKind::Warning,
        AgentNoticeKind::Unknown,
    ] {
        let area = Rect::new(0, 0, 80, 20);
        let mut buffer = Buffer::empty(area);
        let notice = notice("n1", kind, "pane_2", "tab_2");
        let endpoint = ClientEndpointId::Local;
        let hits = render_agent_cards(
            &mut buffer,
            area,
            0,
            &[AgentCardView {
                endpoint_id: &endpoint,
                notice: &notice,
            }],
            NOW,
            &palette,
        );
        let card = hits
            .iter()
            .find(|(_, hit)| matches!(hit, AgentCardHit::Card { .. }))
            .map(|(rect, _)| *rect)
            .expect("card");
        assert_eq!(
            card,
            Rect::new(80 - CARD_WIDTH, 0, CARD_WIDTH, 5),
            "{kind:?}"
        );
        let (glyph, accent) = kind_accent(kind, &palette);
        let bar = &buffer[(card.x + 1, card.y + 1)];
        assert_eq!((bar.symbol(), bar.fg), ("▌", accent), "{kind:?}");
        let glyph_cell = &buffer[(card.x + 3, card.y + 1)];
        assert_eq!(
            (glyph_cell.symbol(), glyph_cell.fg),
            (glyph, accent),
            "{kind:?}"
        );
        assert_eq!(glyph_cell.bg, palette.panel_bg);
        let text = buffer_text(&buffer);
        for expected in [
            "agent-n1 · tab tab_2",
            "2m  ×",
            "title n1",
            "body n1",
            "╭",
            "╯",
        ] {
            assert!(
                text.contains(expected),
                "{kind:?} missing {expected:?}\n{text}"
            );
        }
        // × comes first so it wins over the card under the same cell
        assert!(matches!(hits[0].1, AgentCardHit::Close { .. }));
        assert!(card.contains(hits[0].0.as_position()));
    }
    assert_ne!(
        kind_accent(AgentNoticeKind::Question, &palette).1,
        kind_accent(AgentNoticeKind::Done, &palette).1
    );
    assert_eq!(
        kind_accent(AgentNoticeKind::Unknown, &palette),
        kind_accent(AgentNoticeKind::Info, &palette)
    );
}

#[test]
fn kinds_take_theme_colors() {
    let mut light = Config::default();
    light.theme.name = Some("catppuccin-latte".into());
    for palette in [
        default_palette(),
        crate::app::client_palette_from_config(&light),
    ] {
        let accent = |kind| kind_accent(kind, &palette).1;
        assert_eq!(accent(AgentNoticeKind::Done), palette.green);
        assert_eq!(accent(AgentNoticeKind::Info), palette.blue);
        assert_eq!(accent(AgentNoticeKind::Unknown), palette.blue);
        assert_eq!(accent(AgentNoticeKind::Question), palette.red);
        assert_eq!(accent(AgentNoticeKind::Warning), palette.red);
        // question and warning share red; the glyph tells them apart
        assert_ne!(
            kind_accent(AgentNoticeKind::Question, &palette).0,
            kind_accent(AgentNoticeKind::Warning, &palette).0
        );
    }
    assert_ne!(
        default_palette(),
        crate::app::client_palette_from_config(&light),
        "the light theme is a different palette"
    );
}

#[test]
fn the_card_chrome_follows_the_theme() {
    let mut light = Config::default();
    light.theme.name = Some("catppuccin-latte".into());
    let palette = crate::app::client_palette_from_config(&light);
    let area = Rect::new(0, 0, 80, 20);
    let mut buffer = Buffer::empty(area);
    let notice = notice("n1", AgentNoticeKind::Done, "pane_2", "tab_2");
    let endpoint = ClientEndpointId::Local;
    let hits = render_agent_cards(
        &mut buffer,
        area,
        0,
        &[AgentCardView {
            endpoint_id: &endpoint,
            notice: &notice,
        }],
        NOW,
        &palette,
    );
    let card = hits[1].0;
    let corner = &buffer[(card.x, card.y)];
    assert_eq!(
        (corner.symbol(), corner.fg, corner.bg),
        ("╭", palette.surface1, palette.panel_bg)
    );
    let name = &buffer[(card.x + 5, card.y + 1)];
    assert_eq!((name.symbol(), name.fg), ("a", palette.text));
    let body = &buffer[(card.x + 3, card.y + 3)];
    assert_eq!((body.symbol(), body.fg), ("b", palette.overlay1));
}

fn teamed(mut notice: AgentNoticeInfo, team: &str, role: Option<&str>) -> AgentNoticeInfo {
    notice.team = Some(team.into());
    notice.role = role.map(str::to_string);
    notice
}

#[test]
fn team_label_sits_in_the_top_border_and_in_the_banner() {
    let palette = default_palette();
    let endpoint = ClientEndpointId::Local;
    let notice = teamed(
        notice("n1", AgentNoticeKind::Done, "pane_2", "tab_2"),
        "fix sync",
        Some("fixer"),
    );
    let views = [AgentCardView {
        endpoint_id: &endpoint,
        notice: &notice,
    }];
    let area = Rect::new(0, 0, 80, 20);
    let mut buffer = Buffer::empty(area);
    let hits = render_agent_cards(&mut buffer, area, 0, &views, NOW, &palette);
    let card = hits[1].0;
    assert_eq!(card.height, 5, "the team adds no row");
    let top: String = (card.x..card.right())
        .map(|x| buffer[(x, card.y)].symbol().to_string())
        .collect();
    assert!(top.starts_with("╭─ ◆ fix sync · fixer ─"), "{top}");
    assert!(top.ends_with("─╮"), "{top}");
    let diamond = &buffer[(card.x + 3, card.y)];
    assert_eq!((diamond.symbol(), diamond.fg), ("◆", palette.accent));
    let team = &buffer[(card.x + 5, card.y)];
    assert_eq!((team.symbol(), team.fg), ("f", palette.text));
    assert!(team.modifier.contains(Modifier::BOLD));

    // a long team is cut inside the border
    let long = teamed(notice.clone(), &"x".repeat(80), None);
    let mut buffer = Buffer::empty(area);
    render_agent_cards(
        &mut buffer,
        area,
        0,
        &[AgentCardView {
            endpoint_id: &endpoint,
            notice: &long,
        }],
        NOW,
        &palette,
    );
    let top: String = (card.x..card.right())
        .map(|x| buffer[(x, card.y)].symbol().to_string())
        .collect();
    assert!(top.ends_with("x… ───╮"), "{top}");

    // the narrow banner: ◆ team · role before the sender
    let mut buffer = Buffer::empty(Rect::new(0, 0, 60, 3));
    let hits =
        render_agent_card_banner(&mut buffer, Rect::new(0, 0, 60, 3), &views, false, &palette);
    let rect = hits[0].0;
    let line: String = (rect.x..rect.right())
        .map(|x| buffer[(x, rect.y)].symbol().to_string())
        .collect();
    assert!(
        line.starts_with(" ✓ ◆ fix sync · fixer · agent-n1: title n1"),
        "{line}"
    );
    assert_eq!(buffer[(rect.x + 1, rect.y)].fg, palette.green);
    let diamond = &buffer[(rect.x + 3, rect.y)];
    assert_eq!((diamond.symbol(), diamond.fg), ("◆", palette.accent));
}

#[test]
fn no_team_keeps_the_plain_border() {
    let palette = default_palette();
    let endpoint = ClientEndpointId::Local;
    let notice = notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2");
    let views = [AgentCardView {
        endpoint_id: &endpoint,
        notice: &notice,
    }];
    let area = Rect::new(0, 0, 80, 20);
    let mut buffer = Buffer::empty(area);
    let hits = render_agent_cards(&mut buffer, area, 0, &views, NOW, &palette);
    let card = hits[1].0;
    let top: String = (card.x..card.right())
        .map(|x| buffer[(x, card.y)].symbol().to_string())
        .collect();
    assert_eq!(
        top,
        format!("╭{}╮", "─".repeat(usize::from(card.width) - 2))
    );
    let mut buffer = Buffer::empty(Rect::new(0, 0, 60, 3));
    let hits =
        render_agent_card_banner(&mut buffer, Rect::new(0, 0, 60, 3), &views, false, &palette);
    let rect = hits[0].0;
    let line: String = (rect.x..rect.right())
        .map(|x| buffer[(x, rect.y)].symbol().to_string())
        .collect();
    assert!(line.starts_with(" ● agent-n1: title n1"), "{line}");
    // a team with an empty name is no team
    let empty = teamed(notice.clone(), "", Some("fixer"));
    assert_eq!(super::super::agent_cards::team_label(&empty), None);
}

#[test]
fn bodies_wrap_to_two_lines_and_cards_beyond_four_fold() {
    assert_eq!(body_lines("one\ntwo\nthree", 20), vec!["one", "two…"]);
    assert_eq!(body_lines("abcdefgh", 4), vec!["abcd", "efgh"]);
    assert_eq!(body_lines("abcdefghij", 4), vec!["abcd", "efg…"]);
    assert!(body_lines("", 10).is_empty());

    let endpoint = ClientEndpointId::Local;
    let notices: Vec<AgentNoticeInfo> = (1..=6)
        .map(|n| notice(&format!("n{n}"), AgentNoticeKind::Info, "pane_2", "tab_2"))
        .collect();
    let views: Vec<AgentCardView<'_>> = notices
        .iter()
        .map(|notice| AgentCardView {
            endpoint_id: &endpoint,
            notice,
        })
        .collect();
    let area = Rect::new(0, 1, 100, 40);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 41));
    let hits = render_agent_cards(&mut buffer, area, 0, &views, NOW, &default_palette());
    let cards = hits
        .iter()
        .filter(|(_, hit)| matches!(hit, AgentCardHit::Card { .. }))
        .count();
    assert_eq!(cards, 4);
    let (fold, hit) = hits.last().expect("fold");
    assert_eq!(*hit, AgentCardHit::Fold);
    assert!(buffer_text(&buffer).contains("+2 more from agents"));
    assert_eq!(fold.right(), 100);
    // cards start at the area's top (under the tab bar)
    assert_eq!(hits[1].0.y, 1);

    // a short area: fewer cards (five rows each) and still the fold line
    let short = Rect::new(0, 0, 100, 10);
    let mut buffer = Buffer::empty(short);
    let hits = render_agent_cards(&mut buffer, short, 0, &views, NOW, &default_palette());
    assert_eq!(
        hits.iter()
            .filter(|(_, hit)| matches!(hit, AgentCardHit::Card { .. }))
            .count(),
        1
    );
    assert!(hits.iter().all(|(rect, _)| rect.bottom() <= 10));
    assert!(buffer_text(&buffer).contains("+5 more from agents"));
}

#[test]
fn the_first_payload_seeds_and_new_ids_ring_per_kind() {
    let mut state = state();
    let local = ClientEndpointId::Local;
    let (effects, repaint) = state.receive_agent_notices(
        &local,
        payload(
            3,
            true,
            vec![notice("n1", AgentNoticeKind::Question, "pane_2", "tab_2")],
        ),
    );
    assert!(repaint);
    assert!(
        effects.is_empty(),
        "the first payload after attach rings nothing"
    );

    let (effects, _) = state.receive_agent_notices(
        &local,
        payload(
            4,
            false,
            vec![
                notice("n1", AgentNoticeKind::Question, "pane_2", "tab_2"),
                notice("n2", AgentNoticeKind::Done, "pane_3", "tab_3"),
            ],
        ),
    );
    assert_eq!(sounds(&effects), [crate::sound::Sound::Done]);

    let (effects, _) = state.receive_agent_notices(
        &local,
        payload(
            5,
            false,
            vec![notice("n3", AgentNoticeKind::Question, "pane_2", "tab_2")],
        ),
    );
    assert_eq!(sounds(&effects), [crate::sound::Sound::Request]);

    for (revision, kind) in [(6, AgentNoticeKind::Info), (7, AgentNoticeKind::Warning)] {
        let (effects, _) = state.receive_agent_notices(
            &local,
            payload(
                revision,
                false,
                vec![notice(&format!("n{revision}"), kind, "pane_2", "tab_2")],
            ),
        );
        assert!(effects.is_empty(), "{kind:?} is silent");
    }

    // a stale or repeated revision changes nothing
    let (effects, repaint) = state.receive_agent_notices(
        &local,
        payload(
            5,
            false,
            vec![notice("n9", AgentNoticeKind::Question, "p", "t")],
        ),
    );
    assert!(effects.is_empty() && !repaint);
    // a reconnect's initial payload never rings, even with a new id
    let (effects, repaint) = state.receive_agent_notices(
        &local,
        payload(
            9,
            true,
            vec![notice("n8", AgentNoticeKind::Question, "pane_2", "tab_2")],
        ),
    );
    assert!(repaint && effects.is_empty());
    // another boot seeds again
    let mut restarted = payload(
        1,
        false,
        vec![notice("n1", AgentNoticeKind::Question, "pane_2", "tab_2")],
    );
    restarted.boot_id = "boot-2".into();
    let (effects, _) = state.receive_agent_notices(&local, restarted);
    assert!(effects.is_empty());
    assert!(
        state.visible_notifications.is_empty(),
        "never a herdr toast"
    );
}

#[test]
fn terminal_delivery_sends_new_cards_out_unless_their_tab_is_in_front() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Terminal;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    let local = ClientEndpointId::Local;
    state.receive_agent_notices(&local, payload(1, true, Vec::new()));
    let (effects, _) = state.receive_agent_notices(
        &local,
        payload(
            2,
            false,
            vec![
                notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2"),
                // tab_1 is the focused tab
                notice("n2", AgentNoticeKind::Info, "pane_1", "tab_1"),
            ],
        ),
    );
    let terminal: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            ClientShellNotificationEffect::Terminal { title, body } => {
                Some((title.clone(), body.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        terminal,
        [(
            "agent-n1: title n1".to_string(),
            Some("body n1".to_string())
        )]
    );
    assert!(state.visible_notifications.is_empty());
}

#[test]
fn cards_ignore_toast_stickiness_and_a_toast_for_their_pane() {
    for sticky in [false, true] {
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.toast_delivery = crate::config::ToastDelivery::Herdr;
        config.toast_delay_seconds = 0;
        config.toast_sticky = sticky;
        let mut state = ClientShellState::new(config);
        state.sidebar_collapsed = true;
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.receive_agent_notices(
            &ClientEndpointId::Local,
            payload(
                1,
                true,
                vec![notice("n1", AgentNoticeKind::Question, "pane_2", "tab_2")],
            ),
        );
        state.receive_notification(
            &ClientEndpointId::Local,
            SemanticNotification {
                kind: SemanticNotificationKind::Custom,
                title: "pane toast".into(),
                body: None,
                sound: None,
                agent: None,
                workspace_id: Some("ws_1".into()),
                tab_id: Some("tab_2".into()),
                pane_id: Some("pane_2".into()),
                position: None,
            },
            std::time::Instant::now(),
        );
        let frame = state.compose(100, 40).expect("frame");
        let text = frame_rows(&frame).join("\n");
        assert!(text.contains("title n1"), "sticky={sticky}\n{text}");
        assert_eq!(card_rect(&state, "n1").right(), 100, "sticky={sticky}");
        assert!(state
            .agent_cards
            .get(&ClientEndpointId::Local)
            .is_some_and(|cards| cards.notices.len() == 1));
    }
}

#[test]
fn a_top_right_toast_pushes_the_cards_down() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Herdr;
    config.toast_delay_seconds = 0;
    config.toast_sticky = true;
    config.toast_position = crate::config::ToastHerdrPosition::TopRight;
    let mut state = ClientShellState::new(config);
    state.sidebar_collapsed = true;
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::Custom,
            title: "update".into(),
            body: None,
            sound: None,
            agent: None,
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            position: None,
        },
        std::time::Instant::now(),
    );
    state.receive_agent_notices(
        &ClientEndpointId::Local,
        payload(
            1,
            true,
            vec![notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2")],
        ),
    );
    state.compose(100, 40).expect("frame");
    let toast = state.hits.notification_toasts[0].0;
    let card = card_rect(&state, "n1");
    assert!(card.y >= toast.bottom(), "{card:?} below {toast:?}");
}

/// FORK.md section 10 runs this one by name: a card reaches the screen and
/// its clicks reach the server.
mod fork_smoke {
    use super::*;

    #[test]
    fn a_click_focuses_the_agent_and_x_or_right_click_dismisses() {
        let mut state = state();
        state.receive_agent_notices(
            &ClientEndpointId::Local,
            payload(
                1,
                true,
                vec![
                    notice("n1", AgentNoticeKind::Question, "pane_2", "tab_2"),
                    notice("n2", AgentNoticeKind::Done, "pane_3", "tab_3"),
                    // on the focused tab: the visit cannot clear it
                    notice("n3", AgentNoticeKind::Info, "pane_1", "tab_1"),
                ],
            ),
        );
        state.compose(100, 40).expect("frame");

        // left click on the card body: focus its pane, the card stays until the
        // server clears it
        let card = card_rect(&state, "n1");
        let outcome = press(&mut state, MouseButton::Left, card.x + 4, card.y + 2);
        assert_eq!(
            requests(&outcome),
            [Method::PaneFocus(crate::api::schema::PaneTarget {
                pane_id: "pane_2".into()
            })]
        );
        assert!(state.agent_cards[&ClientEndpointId::Local]
            .hidden
            .is_empty());

        // × dismisses through the server and hides at once
        let close = close_rect(&state, "n2");
        let outcome = press(&mut state, MouseButton::Left, close.x + 1, close.y);
        assert_eq!(
            requests(&outcome),
            [Method::AgentNoticeDismiss(
                crate::api::schema::AgentNoticeDismissParams {
                    ids: vec!["n2".into()],
                    all: false
                }
            )]
        );
        state.compose(100, 40).expect("frame");
        assert!(!state
            .hits
            .agent_cards
            .iter()
            .any(|(_, hit)| matches!(hit, AgentCardHit::Card { id, .. } if id == "n2")));

        // a card on the tab in front: the click focuses and dismisses
        let card = card_rect(&state, "n3");
        let outcome = press(&mut state, MouseButton::Left, card.x + 4, card.y + 2);
        assert!(matches!(
            &requests(&outcome)[..],
            [Method::PaneFocus(_), Method::AgentNoticeDismiss(params)] if params.ids == ["n3"]
        ));

        // right click anywhere on a card dismisses
        state.compose(100, 40).expect("frame");
        let card = card_rect(&state, "n1");
        let outcome = press(&mut state, MouseButton::Right, card.x + 4, card.y + 2);
        assert!(matches!(
            &requests(&outcome)[..],
            [Method::AgentNoticeDismiss(params)] if params.ids == ["n1"] && !params.all
        ));
        // the press went nowhere else (not to the pane underneath)
        assert_eq!(outcome.actions.len(), 1, "{:?}", outcome.actions.len());
    }
}

#[test]
fn the_fold_line_dismisses_every_card() {
    let mut state = state();
    state.receive_agent_notices(
        &ClientEndpointId::Local,
        payload(
            1,
            true,
            (1..=6)
                .map(|n| notice(&format!("n{n}"), AgentNoticeKind::Info, "pane_2", "tab_2"))
                .collect(),
        ),
    );
    state.compose(100, 40).expect("frame");
    let fold = hit_rect(&state, |hit| *hit == AgentCardHit::Fold);
    let outcome = press(&mut state, MouseButton::Left, fold.x + 2, fold.y);
    assert_eq!(
        requests(&outcome),
        [Method::AgentNoticeDismiss(
            crate::api::schema::AgentNoticeDismissParams {
                ids: Vec::new(),
                all: true
            }
        )]
    );
    state.compose(100, 40).expect("frame");
    assert!(state.hits.agent_cards.is_empty());
}

#[test]
fn without_notice_dismiss_the_x_hides_locally_only() {
    let mut state = state();
    state.set_endpoint_methods(Some(vec!["pane.focus".into(), "tab.focus".into()]));
    state.receive_agent_notices(
        &ClientEndpointId::Local,
        payload(
            1,
            true,
            vec![notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2")],
        ),
    );
    state.compose(100, 40).expect("frame");
    let close = close_rect(&state, "n1");
    let outcome = press(&mut state, MouseButton::Left, close.x + 1, close.y);
    assert!(requests(&outcome).is_empty(), "{:?}", requests(&outcome));
    assert!(
        state.visible_endpoint_notice.is_none(),
        "no unsupported notice"
    );
    state.compose(100, 40).expect("frame");
    assert!(state.hits.agent_cards.is_empty());
    // the same list again keeps it hidden; the server dropping it forgets it
    state.receive_agent_notices(
        &ClientEndpointId::Local,
        payload(
            2,
            false,
            vec![notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2")],
        ),
    );
    assert!(!state.has_agent_cards());
    state.receive_agent_notices(&ClientEndpointId::Local, payload(3, false, Vec::new()));
    assert!(state.agent_cards[&ClientEndpointId::Local]
        .hidden
        .is_empty());
}

#[test]
fn another_machines_card_activates_that_machine_and_hides_locally() {
    let mut state = state();
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    let mut remote_snapshot = snapshot();
    remote_snapshot.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote, Box::new(remote_snapshot));
    let mut list = payload(
        1,
        true,
        vec![notice("n1", AgentNoticeKind::Question, "pane_9", "tab_9")],
    );
    list.boot_id = "remote-boot".into();
    state.receive_agent_notices(&remote, list);
    state.compose(100, 40).expect("frame");

    let card = card_rect(&state, "n1");
    let outcome = press(&mut state, MouseButton::Left, card.x + 4, card.y + 2);
    assert!(requests(&outcome).is_empty());
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::ActivateEndpoint {
            endpoint_id,
            target: Some(ClientEndpointFocusTarget::Pane(pane)),
        } if endpoint_id == &remote && pane == "pane_9"
    )));

    let close = close_rect(&state, "n1");
    let outcome = press(&mut state, MouseButton::Left, close.x + 1, close.y);
    assert!(
        requests(&outcome).is_empty(),
        "only the active machine is asked"
    );
    assert!(!state.has_agent_cards());
}

#[test]
fn cards_from_an_older_boot_are_not_drawn() {
    let mut state = state();
    let mut list = payload(
        1,
        true,
        vec![notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2")],
    );
    list.boot_id = "boot-0".into();
    state.receive_agent_notices(&ClientEndpointId::Local, list);
    assert!(!state.has_agent_cards());
    state.compose(100, 40).expect("frame");
    assert!(state.hits.agent_cards.is_empty());
}

#[test]
fn the_mobile_layout_shows_one_banner_line() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.receive_agent_notices(
        &ClientEndpointId::Local,
        payload(
            1,
            true,
            vec![
                notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2"),
                notice("n2", AgentNoticeKind::Question, "pane_3", "tab_3"),
            ],
        ),
    );
    let frame = state.compose(44, 30).expect("mobile frame");
    let rows = frame_rows(&frame);
    let (row, line) = rows
        .iter()
        .enumerate()
        .find(|(_, row)| row.contains("agent-n2: title n2"))
        .expect("banner");
    assert!(line.contains("+1 more"), "{line}");
    assert!(line.starts_with(" ?"), "{line}");
    assert_eq!(state.hits.agent_cards.len(), 1);
    assert_eq!(state.hits.agent_cards[0].0.y, row as u16);
}

#[test]
fn surface_patches_only_fall_back_when_a_row_lands_on_a_card() {
    let mut state = state();
    state.compose(100, 40).expect("frame");
    let patch = |y: u16| crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y,
            cells: vec![surface().frame.cells[0].clone(); 100],
        }],
        panes: Vec::new(),
        cursor: None,
    };
    // no cards: the early exit, whatever the rows
    assert!(!agent_cards_block_patch(&state, &patch(0)));

    state.receive_agent_notices(
        &ClientEndpointId::Local,
        payload(
            1,
            true,
            vec![notice("n1", AgentNoticeKind::Info, "pane_2", "tab_2")],
        ),
    );
    state.compose(100, 40).expect("frame");
    let card = card_rect(&state, "n1");
    let pane_top = state.layout(100, 40).pane_surface.y;
    assert!(agent_cards_block_patch(&state, &patch(card.y - pane_top)));
    assert!(!agent_cards_block_patch(
        &state,
        &patch(card.bottom() - pane_top + 2)
    ));
}

#[test]
fn a_machine_removed_from_the_list_takes_its_cards_along() {
    let mut state = state();
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    let mut list = payload(
        1,
        true,
        vec![notice("n1", AgentNoticeKind::Info, "pane_9", "tab_9")],
    );
    list.boot_id = "remote-boot".into();
    state.receive_agent_notices(&remote, list);
    assert!(state.has_agent_cards(), "no snapshot yet: shown");
    state.set_endpoint_catalog(&[]);
    assert!(!state.has_agent_cards());
}
