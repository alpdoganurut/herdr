//! Fork (perf watch): the `[ui] perf_readout` corner readout: nothing polls or
//! draws while it is off; on, it polls `server.perf_summary` at most once a
//! second from a server that advertises it and draws in the corner.

use std::time::{Duration, Instant};

use super::*;
use crate::api::schema::perf::{ServerPerfLoopInfo, ServerPerfSummaryInfo};
use crate::api::schema::{Method, ResponseResult};
use crate::config::Config;

const COLS: u16 = 100;
const ROWS: u16 = 20;

fn state_with(perf_readout: bool, methods: Option<Vec<String>>) -> ClientShellState {
    let mut config = Config::default();
    config.ui.perf_readout = perf_readout;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(methods);
    state
}

fn advertised() -> Option<Vec<String>> {
    Some(vec!["tab.focus".into(), "server.perf_summary".into()])
}

fn tick(state: &mut ClientShellState, now: Instant) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_perf_readout(now, &mut outcome);
    outcome
}

fn perf_polls(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. }
                if matches!(request.method, Method::ServerPerfSummary(_)) =>
            {
                Some(request.id.clone())
            }
            _ => None,
        })
        .collect()
}

fn bottom_row(state: &mut ClientShellState) -> String {
    let frame = state.compose(COLS, ROWS).expect("composed frame").frame;
    let y = ROWS - 1;
    (0..COLS)
        .map(|x| frame.cells[(y * frame.width + x) as usize].symbol.as_str())
        .collect::<String>()
}

fn perf(full_renders: u64, full_render_total_us: u64, stalls: u64) -> ServerPerfSummaryInfo {
    ServerPerfSummaryInfo {
        pid: 4242,
        started_unix_ms: 1,
        generation: 0,
        window_ms: 1_000,
        frames_sent: 0,
        frame_bytes: 0,
        frames_deferred: 0,
        full_renders,
        full_render_total_us,
        full_render_max_us: 0,
        retained_renders: 0,
        retained_fallbacks: 0,
        sync_holds: 0,
        sync_hold_timeouts: 0,
        loops: vec![ServerPerfLoopInfo {
            name: "server_loop".into(),
            threshold_ms: 250,
            stalls,
            longest_stall_ms: 0,
            busy_ms: 0,
        }],
        clients: Vec::new(),
    }
}

#[test]
fn fork_smoke_perf_readout_off_polls_and_draws_nothing() {
    let mut state = state_with(false, advertised());
    let plain = bottom_row(&mut state);
    let start = Instant::now();
    for second in 0..5 {
        let outcome = tick(&mut state, start + Duration::from_secs(second));
        assert!(outcome.actions.is_empty(), "no poll while off");
        assert!(!outcome.repaint, "no repaint while off");
    }
    assert_eq!(state.perf_readout.polls, 0);
    assert_eq!(bottom_row(&mut state), plain, "nothing drawn");
    assert!(!plain.contains("fps"));
    assert!(state.perf_readout.drawn.is_none());
}

#[test]
fn fork_smoke_perf_readout_shows_fps_and_polls_at_most_once_a_second() {
    let mut state = state_with(true, advertised());
    let start = Instant::now();

    // The first tick starts the window and polls once.
    let first = perf_polls(&tick(&mut state, start));
    assert_eq!(first.len(), 1);
    // A poll in flight is not repeated, and nothing happens within the second.
    assert!(tick(&mut state, start + Duration::from_millis(500))
        .actions
        .is_empty());
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &first[0],
        Ok(ResponseResult::ServerPerfSummary {
            perf: perf(10, 30_000, 0),
        }),
    );
    assert!(!repaint, "no text before the first full second");

    for _ in 0..58 {
        crate::perf::record_client_frame(1_000_000);
    }
    crate::perf::record_client_frame(4_100_000);
    let outcome = tick(&mut state, start + Duration::from_secs(1));
    assert!(outcome.repaint);
    let second = perf_polls(&outcome);
    assert_eq!(second.len(), 1, "one poll a second");
    let row = bottom_row(&mut state);
    assert!(row.trim_end().ends_with("59 fps · 4.1 ms"), "{row:?}");
    assert!(state.perf_readout.drawn.is_some());

    // The next answer of the same server run adds its render time (20 renders
    // in 50 ms) and a stall.
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &second[0],
        Ok(ResponseResult::ServerPerfSummary {
            perf: perf(30, 80_000, 1),
        }),
    );
    assert!(repaint);
    let row = bottom_row(&mut state);
    assert!(
        row.trim_end()
            .ends_with("59 fps · 4.1 ms · srv 2.5 ms · 1 stall"),
        "{row:?}"
    );
    assert_eq!(state.perf_readout.polls, 2);

    // Turned off live: cleared once, then nothing.
    state.config.perf_readout = false;
    assert!(tick(&mut state, start + Duration::from_secs(2)).repaint);
    assert!(!bottom_row(&mut state).contains("fps"));
    assert!(!tick(&mut state, start + Duration::from_secs(3)).repaint);
}

#[test]
fn fork_smoke_perf_readout_never_polls_a_server_without_the_method() {
    for methods in [Some(vec!["tab.focus".to_owned()]), None] {
        let mut state = state_with(true, methods);
        let start = Instant::now();
        for second in 0..3 {
            let outcome = tick(&mut state, start + Duration::from_secs(second));
            assert!(perf_polls(&outcome).is_empty());
        }
        assert!(state.visible_endpoint_notice.is_none(), "no notice");
        // The client half still shows.
        assert!(bottom_row(&mut state).contains("fps"));
    }
}

#[test]
fn fork_smoke_perf_readout_a_failed_poll_raises_no_notice() {
    let mut state = state_with(true, advertised());
    let start = Instant::now();
    let polls = perf_polls(&tick(&mut state, start));
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &polls[0],
        Err(ClientShellEndpointError {
            code: Some("invalid_request".into()),
            message: "invalid request: unknown variant `server.perf_summary`".into(),
        }),
    );
    assert!(!repaint && actions.is_empty());
    assert!(state.visible_endpoint_notice.is_none());
    // The poll ended, so the next second polls again.
    assert_eq!(
        perf_polls(&tick(&mut state, start + Duration::from_secs(1))).len(),
        1
    );
}

#[test]
fn fork_smoke_perf_readout_polls_again_after_the_projection_resets() {
    let mut state = state_with(true, advertised());
    let start = Instant::now();
    let first = perf_polls(&tick(&mut state, start));
    assert_eq!(first.len(), 1, "a poll is in flight");
    let (_, _) = state.handle_endpoint_result(
        "boot-1",
        &first[0],
        Ok(ResponseResult::ServerPerfSummary {
            perf: perf(10, 30_000, 2),
        }),
    );
    let second = perf_polls(&tick(&mut state, start + Duration::from_secs(1)));
    assert_eq!(second.len(), 1);
    assert!(bottom_row(&mut state).contains("2 stalls"));

    // A reconnect drops the request in flight (it never answers) and the old
    // server's numbers; the readout polls the new one at the next second.
    state.reset_endpoint_projection();
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(advertised());
    let third = perf_polls(&tick(&mut state, start + Duration::from_secs(2)));
    assert_eq!(third.len(), 1, "polls again after the reset");
    assert!(!bottom_row(&mut state).contains("stall"), "no old numbers");
}

#[test]
fn fork_smoke_perf_a_patch_that_writes_nothing_is_not_a_presented_frame() {
    let mut state = state_with(false, advertised());
    let mut client = crate::client::state::ClientState::test_new();
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let cursor = frame.frame.cursor.clone();
    let mut output = Vec::new();
    assert!(client.try_present_frame_to(&mut output, frame));
    let presented = crate::perf::client::FRAMES_PRESENTED.get();

    // A metadata-only patch (no rows, same cursor) encodes to no bytes.
    let patch = crate::client::shell::ClientComposedSurfacePatch {
        rows: Vec::new(),
        cursor,
    };
    assert!(client.present_surface_patch(patch).expect("presented"));
    assert_eq!(
        crate::perf::client::FRAMES_PRESENTED.get(),
        presented,
        "nothing was written, so no frame is counted"
    );
}
