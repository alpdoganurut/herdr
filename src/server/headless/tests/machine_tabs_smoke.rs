//! Fork smoke test for machine tabs: a fork client federated with two fork
//! servers. The shell of the machine that is not shown (its surface is
//! inactive) still gets the shell snapshot and the fork's optional pushes,
//! and the client keeps the two machines' apart. The names carry
//! `fork_smoke` so the sync gate's `-E 'test(fork_smoke)'` runs them.

use super::teams_smoke::{client_api, teams_server};
use super::*;
use crate::api::schema::{
    ClientShellSurfaceSetParams, Method, Request, TabSetDndParams, TabSetPinnedParams,
    TeamMakeParams,
};
use crate::client::endpoint::{
    accepts_endpoint_message, decode_endpoint_control, ClientEndpointId, ClientEndpointStatus,
    EndpointControlMessage, ProfileId, SavedSshEndpoint,
};
use crate::client::{ClientShellConfig, ClientShellState};

/// The client of this machine on the server, its surface inactive.
const FEDERATED: u64 = 71;
/// A client attached on the server's own machine.
const ATTACHED: u64 = 51;

type Control = std::sync::mpsc::Receiver<Vec<u8>>;

struct Machine {
    server: HeadlessServer,
    group_id: String,
    tab_ids: Vec<String>,
}

/// A server whose ids are another server's too (each real one counts from
/// `w1`): the bucket `w901` and the group `w902` with two agents.
fn machine(boot_id: &str) -> Machine {
    let (mut server, _, _) = teams_server();
    server.client_shell_boot_id = boot_id.into();
    for (workspace, id) in server.app.state.workspaces.iter_mut().zip(["w901", "w902"]) {
        workspace.id = id.into();
        workspace.custom_name = Some(format!("{boot_id} {id}"));
    }
    let group_id = server.app.state.workspaces[1].id.clone();
    let tab_ids = (0..2)
        .map(|tab| server.app.public_tab_id(1, tab).expect("tab id"))
        .collect();
    Machine {
        server,
        group_id,
        tab_ids,
    }
}

fn connect(
    server: &mut HeadlessServer,
    client_id: u64,
    surface_active: bool,
) -> (Control, Control) {
    let (writer, control, render) = test_client_writer();
    server.handle_server_event(ServerEvent::ClientShellConnected {
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        client_id,
        surface_cols: 80,
        surface_rows: 23,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: false,
        surface_active,
        writer,
    });
    (control, render)
}

/// An API request the way the server loop takes it: queued, drained, and
/// rendered when the drain asks for a full pass.
fn api_turn(server: &mut HeadlessServer, method: Method) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    server.app.api_rx = receiver;
    let (respond_to, response) = std::sync::mpsc::channel();
    sender
        .send(api::ApiRequestMessage {
            request: Request {
                id: "machine-tabs".into(),
                method,
            },
            respond_to,
            response_write_complete: None,
            peer_pid: None,
        })
        .expect("queued");
    assert!(
        server.drain_api_requests_with_shutdown_check(),
        "the loop renders after an API request, whoever is attached"
    );
    let reply: serde_json::Value =
        serde_json::from_str(&response.recv().expect("reply")).expect("json");
    assert!(reply.get("error").is_none(), "{reply}");
    server.render_and_stream();
}

fn pin(machine: &mut Machine, tab: usize, pinned: bool) {
    let tab_id = machine.tab_ids[tab].clone();
    api_turn(
        &mut machine.server,
        Method::TabSetPinned(TabSetPinnedParams { tab_id, pinned }),
    );
}

/// Every message until the stream has been quiet for a while (the test
/// writer forwards through a thread that blocks on the one-slot render
/// channel, so that is drained too), and how many pane frames came.
fn drain((control, render): &(Control, Control)) -> (Vec<ServerMessage>, usize) {
    let mut messages = Vec::new();
    let mut frames = 0;
    let mut quiet = 0;
    while quiet < 4 {
        while render.try_recv().is_ok() {
            frames += 1;
        }
        match control.recv_timeout(Duration::from_millis(50)) {
            Ok(bytes) => {
                messages.push(read_server_message(bytes));
                quiet = 0;
            }
            Err(_) => quiet += 1,
        }
    }
    (messages, frames)
}

/// What a machine that is not shown sent, through the client's own gate for
/// one and its decoder, and how many pane frames came.
fn received(client: &(Control, Control)) -> (Vec<EndpointControlMessage>, usize) {
    let (messages, frames) = drain(client);
    let messages = messages
        .into_iter()
        .filter_map(|message| {
            let accepted = accepts_endpoint_message(false, false, false, &message);
            match message {
                ServerMessage::EndpointControl { kind, data } => {
                    assert!(accepted, "{kind} is taken while its machine is not shown");
                    Some(decode_endpoint_control(&kind, &data).expect("decodes"))
                }
                _ => None,
            }
        })
        .collect();
    (messages, frames)
}

/// Feed what an endpoint sent into the client; returns the kinds it took.
fn feed(
    shell: &mut ClientShellState,
    endpoint_id: &ClientEndpointId,
    messages: Vec<EndpointControlMessage>,
) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    for message in messages {
        match message {
            EndpointControlMessage::Snapshot(snapshot) => {
                shell.set_endpoint_status(endpoint_id, ClientEndpointStatus::Online);
                shell.cache_endpoint_snapshot_inactive_for_generation(endpoint_id, 1, snapshot);
                kinds.push("snapshot");
            }
            EndpointControlMessage::TabPins(payload) => {
                shell.receive_tab_pins(endpoint_id, payload);
                kinds.push("tab-pins");
            }
            EndpointControlMessage::Teams(payload) => {
                shell.receive_teams(endpoint_id, payload);
                kinds.push("teams");
            }
            EndpointControlMessage::Quiet(payload) => {
                shell.receive_quiet(endpoint_id, payload);
                kinds.push("quiet");
            }
            _ => {}
        }
    }
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// The frame with `endpoint_id` shown (its tabs sidebar; no pane surface).
fn shown(shell: &mut ClientShellState, endpoint_id: &ClientEndpointId) -> String {
    assert!(shell.activate_endpoint_projection(endpoint_id));
    frame_text(&shell.compose(120, 40).expect("frame").frame)
}

const ALL: [&str; 4] = ["quiet", "snapshot", "tab-pins", "teams"];

#[tokio::test]
async fn fork_smoke_a_machine_not_shown_still_pushes_and_stays_apart() {
    let mut local = machine("alpha");
    let mut remote = machine("beta");
    assert_eq!(local.tab_ids, remote.tab_ids, "the ids collide on purpose");

    // The remote's shell is attached before anything was pushed, inactive.
    let early = connect(&mut remote.server, FEDERATED, false);
    remote.server.render_and_stream();
    for (machine, pinned) in [(&mut local, 0), (&mut remote, 1)] {
        pin(machine, pinned, true);
        let tab_id = machine.tab_ids[pinned].clone();
        api_turn(
            &mut machine.server,
            Method::TabSetDnd(TabSetDndParams { tab_id, dnd: true }),
        );
        let _attached = connect(&mut machine.server, ATTACHED, true);
        let purpose = format!("{} purpose", machine.server.client_shell_boot_id);
        let made = client_api(
            &mut machine.server,
            ATTACHED,
            Method::TeamMake(TeamMakeParams {
                workspace_id: machine.group_id.clone(),
                purpose: Some(purpose),
                caller_pane: None,
            }),
        );
        assert_eq!(made["result"]["type"], "team_reply", "{made}");
        machine.server.render_and_stream();
    }
    // And one that attaches once everything exists.
    let late = connect(&mut remote.server, FEDERATED + 1, false);
    remote.server.render_and_stream();
    let own = connect(&mut local.server, FEDERATED, true);
    local.server.render_and_stream();

    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").expect("profile id"),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote_id = ClientEndpointId::Ssh(profile.id.clone());
    let local_id = ClientEndpointId::Local;
    for (client, first) in [(&early, true), (&late, false)] {
        let mut shell = ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        shell.set_endpoint_catalog(std::slice::from_ref(&profile));
        let (messages, frames) = received(client);
        assert_eq!(frames, 0, "no pane frames for a machine that is not shown");
        assert_eq!(feed(&mut shell, &remote_id, messages), ALL);
        if first {
            let (messages, frames) = received(&own);
            assert!(frames > 0, "the machine shown sends its panes");
            assert_eq!(feed(&mut shell, &local_id, messages), ALL);
            for (endpoint_id, name, other) in [
                (&local_id, "alpha", "beta"),
                (&remote_id, "beta", "alpha"),
                (&local_id, "alpha", "beta"),
            ] {
                let text = shown(&mut shell, endpoint_id);
                assert!(text.contains(&format!("{name} purpose")), "{text}");
                assert!(!text.contains(other), "{text}");
                assert!(text.contains("Pinned"), "{text}");
            }
        } else {
            let text = shown(&mut shell, &remote_id);
            assert!(text.contains("beta purpose"), "{text}");
        }
    }
    // Shown, then left again: later changes still arrive.
    let boot_id = remote.server.client_shell_boot_id.clone();
    for active in [true, false] {
        remote
            .server
            .handle_server_event(ServerEvent::ClientShellEndpointRequest {
                client_id: FEDERATED,
                boot_id: boot_id.clone(),
                request: Box::new(Request {
                    id: format!("surface-{active}"),
                    method: Method::ClientShellSurfaceSet(ClientShellSurfaceSetParams { active }),
                }),
            });
        remote.server.render_and_stream();
    }
    drain(&early);
    pin(&mut remote, 0, true);
    let pins = received(&early)
        .0
        .into_iter()
        .filter_map(|message| match message {
            EndpointControlMessage::TabPins(payload) => Some(payload),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].boot_id, "beta");
    assert_eq!(pins[0].tab_ids, remote.tab_ids);
}
