//! Fork: one shell client's size changes resize the tab it shows, not every
//! tab. The info dock is per tab, so the client's surface width changes on a
//! dock toggle and on a switch between tabs with and without it; sizing every
//! tab of every workspace on each of those reflowed and SIGWINCHed every
//! agent of the session. Hidden tabs are sized when shown.

use super::*;

/// One workspace with `tabs` tabs, one pane each, holding `history` bytes.
fn server_with_tabs(tabs: usize, history: &[u8]) -> HeadlessServer {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("tab-geometry");
    for index in 1..tabs {
        workspace.test_add_tab(Some(&format!("tab-{index}")));
    }
    for tab in 0..tabs {
        let pane = workspace.tabs[tab].root_pane;
        workspace.insert_test_runtime(
            pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                80,
                24,
                1024 * 1024,
                history,
            ),
        );
    }
    workspace.switch_tab(0);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    server
}

fn tab_size(server: &HeadlessServer, tab: usize) -> (u16, u16) {
    let workspace = &server.app.state.workspaces[0];
    workspace.test_runtimes[&workspace.tabs[tab].root_pane].current_size()
}

fn content_seqs(server: &HeadlessServer) -> Vec<u64> {
    let workspace = &server.app.state.workspaces[0];
    workspace
        .tabs
        .iter()
        .map(|tab| workspace.test_runtimes[&tab.root_pane].content_seq())
        .collect()
}

fn resize(server: &mut HeadlessServer, client_id: u64, cols: u16, rows: u16) -> bool {
    server.handle_server_event(ServerEvent::ClientShellResize {
        client_id,
        surface_cols: cols,
        surface_rows: rows,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
    })
}

fn focus_tab(server: &mut HeadlessServer, client_id: u64, tab: usize) {
    let tab_id = server.app.public_tab_id(0, tab).unwrap();
    assert!(server.focus_shell_client_on_tab(client_id, &tab_id));
    server.claim_shell_tab_geometry(client_id, false);
}

#[tokio::test]
async fn fork_smoke_single_client_resize_reflows_only_the_shown_tab() {
    let mut server = server_with_tabs(4, b"");
    let (control, _) = connect_test_shell(&mut server, 1, 100, 30);
    let _ = control.recv().expect("snapshot");
    let connected = tab_size(&server, 0);
    let before = content_seqs(&server);

    // The info dock opens on the shown tab: the surface narrows.
    assert!(resize(&mut server, 1, 70, 30));

    let after = content_seqs(&server);
    assert_ne!(tab_size(&server, 0), connected);
    assert_ne!(after[0], before[0]);
    assert_eq!(after[1..], before[1..], "hidden tabs must not be reflowed");
    for tab in 1..4 {
        assert_eq!(tab_size(&server, tab), connected);
    }
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn single_client_hidden_tab_is_sized_when_shown() {
    let mut server = server_with_tabs(3, b"");
    let (control, _) = connect_test_shell(&mut server, 1, 100, 30);
    let _ = control.recv().expect("snapshot");
    focus_tab(&mut server, 1, 1);
    focus_tab(&mut server, 1, 0);
    // A window resize while tab 0 is shown.
    assert!(resize(&mut server, 1, 90, 25));
    let shown = tab_size(&server, 0);
    assert_ne!(tab_size(&server, 1), shown);

    // The client shows tab 1 at the same surface size; it reports it again
    // for the newly shown tab (`shell_runtime`), and only tab 1 is sized.
    focus_tab(&mut server, 1, 1);
    let before = content_seqs(&server);
    resize(&mut server, 1, 90, 25);
    let after = content_seqs(&server);
    assert_eq!(tab_size(&server, 1), shown);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[2], before[2]);

    // Reporting the same size again is a no-op for every pane.
    resize(&mut server, 1, 90, 25);
    assert_eq!(content_seqs(&server), after);
    shutdown_test_runtimes(&mut server);
}

/// Fork: a dock-open tab and a dock-closed tab, switched back and forth, size
/// only the tab being shown, and only on its first show at its width.
#[tokio::test]
async fn fork_smoke_switching_dock_and_plain_tabs_reflows_one_tab_at_most() {
    let mut server = server_with_tabs(6, b"");
    let (control, _) = connect_test_shell(&mut server, 1, 100, 30);
    let _ = control.recv().expect("snapshot");
    // Tab 0 has the dock open (70 cols); tab 1 has not (100 cols).
    assert!(resize(&mut server, 1, 70, 30));
    let mut resized = Vec::new();
    for round in 0..3 {
        for (tab, cols) in [(1, 100), (0, 70)] {
            focus_tab(&mut server, 1, tab);
            let before = content_seqs(&server);
            resize(&mut server, 1, cols, 30);
            let after = content_seqs(&server);
            let changed = (0..before.len())
                .filter(|&index| before[index] != after[index])
                .collect::<Vec<_>>();
            resized.push((round, tab, changed));
        }
    }
    for (round, tab, changed) in resized {
        assert!(
            changed.iter().all(|&index| index == tab),
            "round {round}, tab {tab}: resized {changed:?}"
        );
    }
    for tab in 2..6 {
        assert_eq!(tab_size(&server, tab).1, tab_size(&server, 1).1);
    }
    shutdown_test_runtimes(&mut server);
}

/// Profile: `cargo test --release --bin herdr tab_geometry_resize_profile --
/// --ignored --nocapture`. Times one surface width change (an info dock toggle)
/// with one shell client at 1, 15 and 50 tabs holding 2000 lines each.
#[tokio::test]
#[ignore = "profile"]
async fn tab_geometry_resize_profile() {
    let history = (0..2_000)
        .map(|line| format!("line-{line} with some words to wrap on reflow\r\n"))
        .collect::<String>();
    for tabs in [1, 15, 50] {
        let mut server = server_with_tabs(tabs, history.as_bytes());
        let (control, _) = connect_test_shell(&mut server, 1, 120, 40);
        let _ = control.recv().expect("snapshot");
        let mut samples = Vec::new();
        for index in 0..20 {
            let cols = if index % 2 == 0 { 90 } else { 120 };
            let started = std::time::Instant::now();
            resize(&mut server, 1, cols, 40);
            samples.push(started.elapsed());
        }
        samples.sort();
        eprintln!(
            "tab_geometry_resize tabs={tabs} median={:?} max={:?}",
            samples[samples.len() / 2],
            samples[samples.len() - 1]
        );
        shutdown_test_runtimes(&mut server);
    }
}
