//! Info pane fork smoke test, server side (a child of `fork_smoke`, so the
//! gate's `-E 'test(fork_smoke)'` filter runs it). It drives the notes
//! methods the client dock sends (and the refused checkpoints names) through the client-shell
//! endpoint path (one request in flight per client, the reply as a response
//! chunk), so an upstream change to that path or to the advertised method
//! list breaks it. The notes directory is the per-test temp dir `App::new`
//! picks under `cfg(test)`; the transcript is a synthetic file beside it.

use super::*;
use crate::agent_resume::PersistedAgentSession;
use crate::api::schema::notes::{
    CheckpointKind, CheckpointsAddParams, CheckpointsListParams, NotesAuthor, NotesGetParams,
    NotesSetParams, NotesTarget,
};

/// Removes the test server's notes directory.
struct NotesDirGuard(std::path::PathBuf);

impl Drop for NotesDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Send `method` as client `client_id`'s endpoint command and return the
/// JSON reply carried by its final response chunk.
async fn endpoint(
    server: &mut HeadlessServer,
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
    client_id: u64,
    request_id: &str,
    method: Method,
) -> serde_json::Value {
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id: server.client_shell_boot_id.clone(),
            request: Box::new(Request {
                id: request_id.into(),
                method,
            }),
        })
    );
    let ready = tokio::time::timeout(Duration::from_secs(5), server.server_event_rx.recv())
        .await
        .expect("the endpoint reply is ready in time")
        .expect("the endpoint reply event");
    assert!(!server.handle_server_event(ready));
    loop {
        let bytes = control
            .recv_timeout(Duration::from_secs(5))
            .expect("an endpoint response chunk");
        if let ServerMessage::ClientShellEndpointResponseChunk {
            request_id: got,
            final_chunk,
            data,
            ..
        } = read_server_message(bytes)
        {
            assert_eq!(got, request_id);
            assert!(final_chunk, "notes replies fit one chunk");
            return serde_json::from_slice(&data).expect("the reply is json");
        }
    }
}

fn tab_target(tab_id: &str) -> NotesTarget {
    NotesTarget {
        tab_id: Some(tab_id.into()),
        ..NotesTarget::default()
    }
}

#[tokio::test]
async fn dock_methods_round_trip_over_the_client_shell() {
    let (mut server, _rx) = server_with_claude(Some(SESSION_ID));
    let notes_dir = server
        .app
        .notes
        .store
        .notes_path("x")
        .parent()
        .expect("the notes dir")
        .to_path_buf();
    let _guard = NotesDirGuard(notes_dir.clone());
    fs::create_dir_all(&notes_dir).unwrap();
    let transcript = notes_dir.join("transcript.jsonl");
    fs::write(
        &transcript,
        concat!(
            "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"role\":\"user\",\"content\":\"pick a time crate\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"message\":{\"id\":\"m1\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"jiff, it handles zones\"}]}}\n",
        ),
    )
    .unwrap();
    let terminal_id = root_terminal_id(&server);
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_persisted_agent_session(PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id(SESSION_ID).unwrap(),
            transcript_path: Some(transcript.clone()),
        });
    let tab_id = server.app.public_tab_id(0, 0).expect("the tab's public id");
    let (control, _render) = connect_matching_test_shell(&mut server, 61);
    let _ = control.recv().expect("the initial snapshot");

    // notes.get: the agent session's key, nothing written yet.
    let got = endpoint(
        &mut server,
        &control,
        61,
        "info:get",
        Method::NotesGet(NotesGetParams {
            target: tab_target(&tab_id),
            known_revision: None,
        }),
    )
    .await;
    assert_eq!(got["result"]["type"], "notes_get", "{got}");
    let notes = &got["result"]["notes"];
    assert_eq!(notes["key"], format!("claude-{SESSION_ID}"));
    assert_eq!(notes["revision"], "none");
    assert_eq!(notes["exists"], false);

    // notes.set from the dock: a CAS write against `none`.
    let set = endpoint(
        &mut server,
        &control,
        61,
        "info:set",
        Method::NotesSet(NotesSetParams {
            target: tab_target(&tab_id),
            text: "# plan\n- [ ] pick a crate\n".into(),
            base_revision: Some("none".into()),
            author: NotesAuthor::User,
        }),
    )
    .await;
    assert_eq!(set["result"]["type"], "notes_write", "{set}");
    assert_eq!(set["result"]["write"]["outcome"], "written");
    let revision = set["result"]["write"]["notes"]["revision"]
        .as_str()
        .expect("the new revision")
        .to_owned();
    assert!(revision.starts_with("sha256:"));

    // A poll with the known revision answers `unchanged` without the text.
    let polled = endpoint(
        &mut server,
        &control,
        61,
        "info:poll",
        Method::NotesGet(NotesGetParams {
            target: tab_target(&tab_id),
            known_revision: Some(revision.clone()),
        }),
    )
    .await;
    assert_eq!(polled["result"]["notes"]["unchanged"], true, "{polled}");
    assert!(polled["result"]["notes"]["text"].is_null());

    // The checkpoints.* names stay advertised and refuse: checkpoints
    // were removed.
    for (request_id, method) in [
        (
            "info:add",
            Method::CheckpointsAdd(CheckpointsAddParams {
                target: tab_target(&tab_id),
                kind: CheckpointKind::Bookmark,
                title: "picked jiff".into(),
                detail: None,
                tags: Vec::new(),
                author: NotesAuthor::User,
            }),
        ),
        (
            "info:list",
            Method::CheckpointsList(CheckpointsListParams {
                target: tab_target(&tab_id),
                kinds: Vec::new(),
                since_seq: None,
                limit: None,
            }),
        ),
    ] {
        let refused = endpoint(&mut server, &control, 61, request_id, method).await;
        assert_eq!(refused["error"]["code"], "checkpoints_removed", "{refused}");
    }

    shutdown_test_runtimes(&mut server);
}

fn notes_reveal_payloads(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<crate::server::headless::notes_reveal::NotesRevealPayload> {
    let mut payloads = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == crate::server::headless::notes_reveal::NOTES_REVEAL_KIND {
                payloads.push(
                    crate::server::headless::notes_reveal::NotesRevealPayload::decode(&data)
                        .expect("notes reveal payload decodes"),
                );
            }
        }
    }
    payloads
}

/// Fork (`agents_notes_write reveal=true`): `notes.reveal` reaches every
/// attached client shell once as `endpoint.notes-reveal.v1`, keyed like the
/// shell snapshot's tab row; a client that attaches later gets no replay.
#[tokio::test]
async fn fork_smoke_a_notes_reveal_reaches_every_client_once() {
    let (mut server, _rx) = server_with_claude(Some(SESSION_ID));
    let (first, _first_render) = connect_test_shell(&mut server, 91, 80, 23);
    let (second, _second_render) = connect_test_shell(&mut server, 92, 80, 23);
    server.render_and_stream();
    assert!(
        notes_reveal_payloads(&first).is_empty() && notes_reveal_payloads(&second).is_empty(),
        "nothing before a reveal"
    );

    let tab_id = server.app.public_tab_id(0, 0).expect("tab id");
    let reply = api(
        &mut server,
        Method::NotesReveal(crate::api::schema::notes::NotesRevealParams {
            target: tab_target(&tab_id),
        }),
    );
    assert_eq!(reply["result"]["type"], "ok", "{reply}");
    server.render_and_stream();
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    assert!(snapshot.tabs.iter().any(|tab| tab.tab_id == tab_id));
    for control in [&first, &second] {
        let payloads = notes_reveal_payloads(control);
        assert_eq!(payloads.len(), 1, "one payload per client");
        assert_eq!(payloads[0].tab_id, tab_id, "keyed like the tab row");
        assert_eq!(payloads[0].revision, 1);
    }
    server.render_and_stream();
    assert!(
        notes_reveal_payloads(&first).is_empty(),
        "a reveal is sent once"
    );

    // A client attaching after the reveal does not get it replayed.
    let (late, _late_render) = connect_test_shell(&mut server, 93, 80, 23);
    server.render_and_stream();
    server.render_and_stream();
    assert!(notes_reveal_payloads(&late).is_empty(), "no replay");

    // The next reveal reaches all three.
    api(
        &mut server,
        Method::NotesReveal(crate::api::schema::notes::NotesRevealParams {
            target: tab_target(&tab_id),
        }),
    );
    server.render_and_stream();
    for control in [&first, &second, &late] {
        let payloads = notes_reveal_payloads(control);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].revision, 2);
    }

    // The client shell takes it: the tab's info pane opens without focus.
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    let outcome = shell.receive_notes_reveal(
        &crate::client::endpoint::ClientEndpointId::Local,
        crate::server::headless::notes_reveal::NotesRevealPayload {
            boot_id: "fork-smoke".into(),
            revision: 2,
            tab_id: tab_id.clone(),
        },
    );
    assert!(outcome.repaint);
    assert!(shell.info_dock_open_unfocused_for_test(&tab_id));

    shutdown_test_runtimes(&mut server);
}
