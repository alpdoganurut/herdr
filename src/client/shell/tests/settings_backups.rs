use super::*;

fn settings_frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("settings frame");
    frame_rows(&frame).join("\n")
}

/// Tab through the sections to `backups`.
fn open_backups_section(state: &mut ClientShellState) -> ClientShellInput {
    state.open_settings_overlay();
    let mut outcome = ClientShellInput::default();
    let backups = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::Backups)
        .expect("backups section");
    for _ in 0..backups {
        outcome = state.handle_input_bytes(b"\t");
    }
    outcome
}

fn transcripts_result(
    last_pass: Option<crate::api::schema::AgentTranscriptBackupPass>,
) -> crate::api::schema::ResponseResult {
    crate::api::schema::ResponseResult::AgentTranscripts {
        store_dir: "/home/me/.config/herdr/agent-transcripts".into(),
        enabled: true,
        sessions: 29,
        native_missing: 1,
        transcript_bytes: 2_950_000_000,
        disk_bytes: 3_221_225_472,
        last_pass,
        next_pass_in_ms: Some(150_000),
    }
}

#[test]
fn backups_section_requests_the_store_once_and_renders_it() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    let outcome = open_backups_section(&mut state);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("backups section should request the transcript store");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::AgentTranscripts(_)
    ));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Backups,
            loading_transcripts: true,
            transcripts: None,
            ..
        }))
    ));
    let text = settings_frame_text(&mut state);
    assert!(text.contains("transcript backups"));
    assert!(text.contains("copies of the native transcripts behind open and suspended agents"));
    assert!(text.contains("loading backup status"));
    // Read-only: no apply button, only close.
    assert!(!text.contains("↵ apply"));
    assert!(text.contains("esc close"));

    let request_id = request.id.clone();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(transcripts_result(Some(
            crate::api::schema::AgentTranscriptBackupPass {
                finished_unix: now_unix - 120,
                duration_ms: 145,
                updated: 1,
                unchanged: 28,
                skipped: 0,
                failed: 0,
            },
        ))),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            loading_transcripts: false,
            transcripts: Some(ref store),
            ..
        })) if store.sessions == 29 && store.next_pass_in_ms == Some(150_000)
    ));

    let text = settings_frame_text(&mut state);
    // The countdown ticks from receipt, so a few milliseconds may have passed.
    assert!(text.contains("on · next pass in 2m 30s") || text.contains("on · next pass in 2m 29s"));
    assert!(text.contains("29 (1 whose native transcript is gone)"));
    assert!(text.contains("3.0 GB on disk · 2.7 GB of transcripts"));
    assert!(text.contains("2m ago · took 145 ms"));
    assert!(text.contains("1 copied, 28 unchanged, 0 skipped, 0 failed"));
    assert!(text.contains("/home/me/.config/herdr/agent-transcripts"));

    // Enter does nothing here: the section is read-only.
    let apply = state.handle_input_bytes(b"\r");
    assert!(apply.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Backups,
            ..
        }))
    ));
}

#[test]
fn backups_section_shows_a_disabled_store_and_no_pass_yet() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let outcome = open_backups_section(&mut state);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("backups section should request the transcript store");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::AgentTranscripts {
            store_dir: "/srv/herdr/agent-transcripts".into(),
            enabled: false,
            sessions: 0,
            native_missing: 0,
            transcript_bytes: 0,
            disk_bytes: 0,
            last_pass: None,
            next_pass_in_ms: None,
        }),
    );
    let text = settings_frame_text(&mut state);
    assert!(text.contains("off (session.backup_agent_transcripts)"));
    assert!(text.contains("none since the server started"));
    assert!(text.contains("0 B on disk · 0 B of transcripts"));
}

#[test]
fn unavailable_transcript_store_does_not_wedge_the_backups_section() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(Vec::new()));

    let outcome = open_backups_section(&mut state);
    assert!(outcome.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Backups,
            loading_transcripts: false,
            transcripts: None,
            ..
        }))
    ));
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .is_some_and(|notice| notice.key.code == "agent.transcripts"));
    let text = settings_frame_text(&mut state);
    assert!(text.contains("backup status unavailable"));

    // Leaving and re-entering the section asks again.
    state.handle_input_bytes(b"\t");
    state.set_endpoint_methods(Some(vec!["agent.transcripts".into()]));
    let again = state.handle_input_bytes(b"\x1b[Z");
    assert!(matches!(
        &again.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::AgentTranscripts(_))
    ));
}

/// Fork smoke tests: FORK.md section 10 lists them by name and the sync gate
/// runs them with `-E 'test(fork_smoke)'`.
mod fork_smoke {
    use super::*;

    /// Section tabs are clipped with `.min()` to the fixed 76-column popup; a
    /// longer upstream label or a new upstream section would silently cut
    /// `backups` or `reminders` off. Checked with the integrations badge on,
    /// the widest row (the tabs close up their one-cell gaps to fit it).
    #[test]
    fn every_settings_section_fits_the_76_column_popup() {
        let mut snapshot = snapshot();
        snapshot.integration_updates_available = true;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        state.open_settings_overlay();
        let frame = state.compose(106, 30).expect("settings frame");

        let tabs = state.hits.settings_tabs.clone();
        assert_eq!(
            tabs.iter().map(|(_, section)| *section).collect::<Vec<_>>(),
            ClientSettingsSection::ALL.to_vec(),
            "every section gets a tab, in order"
        );
        assert_eq!(
            &ClientSettingsSection::ALL[ClientSettingsSection::ALL.len() - 2..],
            [
                ClientSettingsSection::Backups,
                ClientSettingsSection::Reminders
            ]
        );
        let popup_left = tabs[0].0.x;
        for (rect, section) in &tabs {
            let label = if *section == ClientSettingsSection::Integrations {
                format!(" ● {} ", section.label())
            } else {
                format!(" {} ", section.label())
            };
            assert_eq!(
                rect.width,
                crate::client::shell::render::display_width(&label),
                "{section:?} tab is clipped"
            );
            assert!(
                rect.right() <= popup_left + 74,
                "{section:?} tab ends past the popup's inner width"
            );
        }
        let row = frame_rows(&frame)[usize::from(tabs[0].0.y)].clone();
        assert!(row.contains(" backups "), "{row}");
        assert!(row.contains(" reminders "), "{row}");
    }
}
