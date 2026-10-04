//! Compose microprofile of the `tabs` sidebar (sidebar v2, S0a).
//!
//! Composes a 160x45 frame with 1, 15 and 75 tabs (five tabs per group,
//! every agent status represented) and prints the median and p95 of
//! `compose` in two cases: a static snapshot (nothing changed since the last
//! frame) and right after a snapshot replacement (derived sidebar state is
//! rebuilt). Manual:
//!
//! `cargo test --release --bin herdr tab_sidebar_compose_profile -- --ignored --nocapture`

use std::hint::black_box;
use std::time::{Duration, Instant};

use super::*;
use crate::config::{Config, SidebarLayoutConfig};

const COLS: u16 = 160;
const ROWS: u16 = 45;
const SAMPLES: usize = 40;
const WARMUP: usize = 5;
const TABS_PER_GROUP: usize = 5;

/// `tabs` tabs in groups of five; the first tab is focused. Statuses cycle
/// working, blocked, idle, done, suspended; every third tab has subagents.
fn perf_snapshot(tabs: usize) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let template = snapshot.workspaces[0].clone();
    let groups = tabs.div_ceil(TABS_PER_GROUP).max(1);
    snapshot.workspaces = (0..groups)
        .map(|index| {
            let mut workspace = template.clone();
            workspace.workspace_id = format!("ws_{index}");
            workspace.number = index + 1;
            workspace.label = format!("group {index}");
            workspace.active_tab_id = format!("t_{}", index * TABS_PER_GROUP);
            workspace.focused = index == 0;
            workspace
        })
        .collect();
    let statuses = [
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Idle,
        AgentStatus::Done,
        AgentStatus::Suspended,
    ];
    snapshot.tabs.clear();
    snapshot.panes.clear();
    snapshot.agents.clear();
    for index in 0..tabs {
        let workspace_id = format!("ws_{}", index / TABS_PER_GROUP);
        let tab_id = format!("t_{index}");
        let pane_id = format!("p_{index}");
        let status = statuses[index % statuses.len()];
        snapshot.tabs.push(ClientShellTab {
            tab_id: tab_id.clone(),
            workspace_id: workspace_id.clone(),
            number: index % TABS_PER_GROUP + 1,
            label: format!("tab number {index}"),
            custom_label: true,
            zoomed: false,
            focused: index == 0,
            agent_status: status,
            color: None,
            important: index % 7 == 1,
            remind_every: None,
        });
        snapshot.panes.push(ClientShellPane {
            pane_id: pane_id.clone(),
            workspace_id: workspace_id.clone(),
            tab_id: tab_id.clone(),
            label: None,
            cwd: Some("/repo".into()),
            foreground_cwd: Some("/repo".into()),
            focused: index == 0,
            right_click_passthrough: false,
        });
        snapshot.agents.push(ClientShellAgent {
            pane_id,
            workspace_id,
            tab_id,
            name: None,
            display_agent: Some("claude".into()),
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: index as u64 + 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: index == 0,
            subagents: if index % 3 == 0 { 2 } else { 0 },
        });
    }
    snapshot.focused_workspace_id = Some("ws_0".into());
    snapshot.focused_tab_id = Some("t_0".into());
    snapshot.focused_pane_id = Some("p_0".into());
    snapshot.tab_bar_right = vec![crate::protocol::ClientShellTabStatusSegment {
        text: "cpu 12%\nmem 80%".into(),
        accent: false,
    }];
    snapshot
}

fn perf_state(tabs: usize) -> ClientShellState {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(perf_snapshot(tabs)));
    state.set_pane_surface(surface());
    let voice: crate::server::headless::voice::VoicePayload =
        serde_json::from_value(serde_json::json!({
            "boot_id": "boot-1",
            "revision": 1,
            "panes": [{ "pane_id": "p_2", "voice": "live" }],
        }))
        .expect("voice payload");
    state.receive_voice(&ClientEndpointId::Local, voice);
    state
}

fn stats(mut samples: Vec<Duration>) -> (u128, u128) {
    samples.sort_unstable();
    (
        samples[samples.len() / 2].as_micros(),
        samples[(samples.len() - 1) * 95 / 100].as_micros(),
    )
}

/// `compose` timings; `replace` sets a fresh snapshot before each frame
/// (outside the timed region).
fn profile(tabs: usize, replace: bool) -> (u128, u128) {
    let mut state = perf_state(tabs);
    let snapshot = perf_snapshot(tabs);
    let mut samples = Vec::with_capacity(SAMPLES);
    for index in 0..WARMUP + SAMPLES {
        if replace {
            state.set_snapshot(Box::new(snapshot.clone()));
        }
        let started = Instant::now();
        black_box(state.compose(COLS, ROWS).expect("composed frame"));
        if index >= WARMUP {
            samples.push(started.elapsed());
        }
    }
    stats(samples)
}

#[test]
#[ignore = "manual tabs sidebar compose profile"]
fn tab_sidebar_compose_profile() {
    println!("tabs sidebar compose at {COLS}x{ROWS}, {SAMPLES} samples");
    println!("        tabs  case             median_us  p95_us");
    for tabs in [1, 15, 75] {
        for (case, replace) in [("static", false), ("snapshot change", true)] {
            let (median, p95) = profile(tabs, replace);
            println!("  {tabs:>10}  {case:<15}  {median:>9}  {p95:>6}");
        }
    }
}
