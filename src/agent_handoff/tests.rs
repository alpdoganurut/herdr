use super::*;

fn run_at(phase: Phase, now: Instant) -> Run {
    Run {
        id: 1,
        agent: HandoffAgent::Claude,
        trigger: Trigger::Auto { percent: 82 },
        settings: RunSettings {
            command: None,
            resume: true,
        },
        phase,
        phase_since: now,
        started_unix: 1_760_350_500,
        session_id: "s-1".into(),
        paths: run_paths(Path::new("/state/handoffs"), "s-1", 1_760_350_500),
        transcript_saved: Some(true),
        kind: None,
        free_since: None,
        write_failures: 0,
        missing_since: None,
        reminded: false,
        blocked_since: None,
        blocked_noted: false,
        check_since: None,
    }
}

fn facts(now: Instant, free: bool) -> Facts {
    Facts {
        now,
        free,
        hook_end: None,
        idle_edge: None,
    }
}

#[test]
fn the_prompt_waits_for_the_copy_and_a_free_agent_and_gives_up_after_a_while() {
    let t0 = Instant::now();
    let mut run = run_at(Phase::AwaitPrompt, t0);
    run.transcript_saved = None;
    assert_eq!(next(&run, &facts(t0, true)), Effect::Wait, "the copy first");
    run.transcript_saved = Some(false);
    assert_eq!(
        next(&run, &facts(t0, true)),
        Effect::TypePrompt,
        "a failed copy still goes on"
    );
    assert_eq!(
        next(&run, &facts(t0, false)),
        Effect::Wait,
        "only when free"
    );
    assert!(matches!(
        next(&run, &facts(t0 + FREE_TIMEOUT, false)),
        Effect::Abort(_)
    ));
}

#[test]
fn the_handoff_turn_ends_on_a_later_stop_hook_or_a_settled_idle_edge() {
    let t0 = Instant::now();
    let typed_at = t0 + Duration::from_secs(1);
    let run = run_at(
        Phase::AwaitHandoff {
            typed_at,
            checked: None,
            checking: false,
        },
        typed_at,
    );
    let now = typed_at + Duration::from_secs(30);
    // An end from before the prompt is the previous turn's.
    let mut f = facts(now, false);
    f.hook_end = Some(t0);
    f.idle_edge = Some(t0);
    assert_eq!(next(&run, &f), Effect::Wait);
    // The Stop hook's end after the prompt: check the file, even while the
    // screen still reads working.
    let end = typed_at + Duration::from_secs(20);
    f.hook_end = Some(end);
    assert_eq!(next(&run, &f), Effect::CheckFile { end });
    // No hooks: an idle edge after the prompt counts once the agent is free.
    let mut f = facts(now, false);
    f.idle_edge = Some(end);
    assert_eq!(next(&run, &f), Effect::Wait, "not settled yet");
    f.free = true;
    assert_eq!(next(&run, &f), Effect::CheckFile { end });
}

#[test]
fn each_turn_end_is_checked_once_and_a_check_in_flight_waits() {
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs(5);
    let mut f = facts(t0 + Duration::from_secs(9), true);
    f.hook_end = Some(end);
    let checked = run_at(
        Phase::AwaitHandoff {
            typed_at: t0,
            checked: Some(end),
            checking: false,
        },
        t0,
    );
    assert_eq!(
        next(&checked, &f),
        Effect::Wait,
        "an empty file: wait for the next end"
    );
    let later = end + Duration::from_secs(2);
    f.hook_end = Some(later);
    assert_eq!(next(&checked, &f), Effect::CheckFile { end: later });
    let checking = run_at(
        Phase::AwaitHandoff {
            typed_at: t0,
            checked: None,
            checking: true,
        },
        t0,
    );
    assert_eq!(next(&checking, &f), Effect::Wait);
}

#[test]
fn a_handoff_that_never_comes_falls_back_to_the_mechanical_one() {
    let t0 = Instant::now();
    let run = run_at(
        Phase::AwaitHandoff {
            typed_at: t0,
            checked: None,
            checking: false,
        },
        t0,
    );
    assert_eq!(
        next(&run, &facts(t0 + HANDOFF_TIMEOUT, false)),
        Effect::WriteMechanical
    );
    assert_eq!(
        next(&run_at(Phase::WritingFallback, t0), &facts(t0, true)),
        Effect::Wait
    );
}

#[test]
fn compact_and_resume_go_in_only_when_free() {
    let t0 = Instant::now();
    let run = run_at(Phase::AwaitCompact, t0);
    assert_eq!(next(&run, &facts(t0, false)), Effect::Wait);
    assert_eq!(next(&run, &facts(t0, true)), Effect::TypeCompact);
    assert!(matches!(
        next(&run, &facts(t0 + FREE_TIMEOUT, false)),
        Effect::Abort(_)
    ));
    let run = run_at(Phase::AwaitResume, t0);
    assert_eq!(next(&run, &facts(t0, false)), Effect::Wait);
    assert_eq!(next(&run, &facts(t0, true)), Effect::TypeResume);
    assert!(matches!(
        next(&run, &facts(t0 + FREE_TIMEOUT, false)),
        Effect::Finish(_)
    ));
}

#[test]
fn compaction_ends_on_a_settled_idle_edge_without_hooks() {
    let t0 = Instant::now();
    let run = run_at(Phase::Compacting { typed_at: t0 }, t0);
    let mut f = facts(t0 + Duration::from_secs(20), true);
    f.idle_edge = Some(t0 + Duration::from_secs(1));
    assert_eq!(next(&run, &f), Effect::Wait, "the command's first frames");
    f.idle_edge = Some(t0 + Duration::from_secs(15));
    assert_eq!(next(&run, &f), Effect::Compacted);
    f.free = false;
    assert_eq!(next(&run, &f), Effect::Wait);
    assert!(matches!(
        next(&run, &facts(t0 + COMPACT_TIMEOUT, false)),
        Effect::Abort(_)
    ));
}

#[test]
fn after_compaction_resumes_unless_off_or_claude_carries_its_own_turn() {
    let t0 = Instant::now();
    let mut run = run_at(Phase::Compacting { typed_at: t0 }, t0);
    assert_eq!(after_compaction(&run), Some(Phase::AwaitResume));
    run.settings.resume = false;
    assert_eq!(after_compaction(&run), None);
    run.settings.resume = true;
    run.trigger = Trigger::External { auto: true };
    assert_eq!(after_compaction(&run), None, "mid-turn auto-compact");
    run.trigger = Trigger::External { auto: false };
    assert_eq!(
        after_compaction(&run),
        Some(Phase::AwaitResume),
        "the user's /compact"
    );
}

#[test]
fn messages_are_held_from_the_prompt_until_the_run_ends() {
    let t0 = Instant::now();
    assert!(!run_at(Phase::AwaitPrompt, t0).holds_messages());
    for phase in [
        Phase::AwaitHandoff {
            typed_at: t0,
            checked: None,
            checking: false,
        },
        Phase::WritingFallback,
        Phase::AwaitCompact,
        Phase::Compacting { typed_at: t0 },
        Phase::AwaitResume,
    ] {
        assert!(run_at(phase, t0).holds_messages(), "{}", phase.name());
    }
}

#[test]
fn the_auto_trigger_fires_once_per_cycle_and_rearms_below_the_threshold() {
    // (armed, percent, at, running) -> (fire, armed)
    assert_eq!(auto_fire(true, 79, 80, false), (false, true));
    assert_eq!(auto_fire(true, 80, 80, false), (true, false));
    // Still over it after firing (the compaction has not shown yet).
    assert_eq!(auto_fire(false, 91, 80, false), (false, false));
    // The compaction showed: armed again.
    assert_eq!(auto_fire(false, 12, 80, false), (false, true));
    assert_eq!(auto_fire(true, 85, 80, false), (true, false));
    // A manual run is on: this cycle is its.
    assert_eq!(auto_fire(true, 85, 80, true), (false, false));
    // 0 never fires.
    assert_eq!(auto_fire(true, 100, 0, false), (false, true));
}

#[test]
fn the_command_template_replaces_or_appends_the_path() {
    let path = Path::new("/s/handoffs/abc/20261013T101500Z/handoff.md");
    assert_eq!(
        render_command(" /leap:handoff --compact {path} ", path),
        "/leap:handoff --compact /s/handoffs/abc/20261013T101500Z/handoff.md"
    );
    assert_eq!(
        render_command("write it to {path}, then {path} again", path),
        format!(
            "write it to {}, then {} again",
            path.display(),
            path.display()
        )
    );
    assert_eq!(
        render_command("/my:handoff", path),
        "/my:handoff /s/handoffs/abc/20261013T101500Z/handoff.md"
    );
}

#[test]
fn the_default_prompt_names_the_path_and_what_to_write() {
    let path = Path::new("/s/h.md");
    let text = default_prompt(path, Some(82));
    assert!(
        text.starts_with("herdr: your context is 82% full"),
        "{text}"
    );
    for part in [
        "/s/h.md",
        "current state",
        "decisions made and why",
        "dead ends and gotchas",
        "files in play",
        "exact next step",
        "/compact",
    ] {
        assert!(text.contains(part), "{part}: {text}");
    }
    assert!(!text.contains('\n'), "one typed line");
    assert!(default_prompt(path, None).starts_with("herdr: your context is compacted next"));
    let t0 = Instant::now();
    let mut run = run_at(Phase::AwaitPrompt, t0);
    assert!(prompt(&run, Some(82)).contains(&run.paths.handoff.display().to_string()));
    run.settings.command = Some("/x {path}".into());
    assert_eq!(
        prompt(&run, Some(82)),
        format!("/x {}", run.paths.handoff.display())
    );
}

#[test]
fn the_file_layout_is_session_then_utc_time_with_a_latest_pointer() {
    assert_eq!(utc_stamp(0), "19700101T000000Z");
    assert_eq!(utc_stamp(1_760_350_500), "20251013T101500Z");
    let paths = run_paths(Path::new("/state/handoffs"), "abc-123", 1_760_350_500);
    assert_eq!(
        paths.dir,
        PathBuf::from("/state/handoffs/abc-123/20251013T101500Z")
    );
    assert_eq!(paths.handoff, paths.dir.join("handoff.md"));
    assert_eq!(paths.transcript, paths.dir.join("transcript.jsonl"));
    assert_eq!(paths.meta, paths.dir.join("meta.json"));
    assert_eq!(
        paths.latest,
        PathBuf::from("/state/handoffs/abc-123/latest.txt")
    );
    assert_eq!(root(Path::new("/state")), PathBuf::from("/state/handoffs"));
    for bad in ["", "../x", ".hidden", "a/b", "a b", &"x".repeat(129)] {
        assert_eq!(session_dir_name(bad), "unknown", "{bad:?}");
    }
    assert_eq!(session_dir_name(" 0f3e_a.b-1 "), "0f3e_a.b-1");
}

#[test]
fn the_injection_and_resume_texts_name_both_files() {
    let handoff = Path::new("/s/h.md");
    let transcript = Path::new("/s/t.jsonl");
    let text = injection(handoff, Some(transcript), Kind::Agent);
    assert!(text.starts_with(INJECTION_PREFIX));
    assert!(text.contains("/s/h.md") && text.contains("/s/t.jsonl"));
    assert!(text.contains("Read the handoff first, then continue the active work"));
    assert!(text.contains("if there is no active work, stop and wait for your user"));
    assert!(!text.contains("written by herdr"));
    let mechanical = injection(handoff, None, Kind::Mechanical);
    assert!(mechanical.contains("written by herdr"));
    assert!(!mechanical.contains("transcript at"));
    let line = resume_line(handoff, transcript);
    assert_eq!(
        line,
        "herdr: compacted \u{b7} handoff /s/h.md \u{b7} transcript /s/t.jsonl \u{2014} read the handoff and continue (or pause if nothing is active)"
    );
}

#[test]
fn the_mechanical_handoff_lists_what_herdr_knows() {
    let text = render_mechanical(&Mechanical {
        agent_name: "rev".into(),
        pane: "w1:p1".into(),
        trigger: "auto".into(),
        reason: "the agent did not write one within 10 minutes".into(),
        written_unix: 1_760_350_500,
        cwd: Some(PathBuf::from("/repo")),
        notes_tail: Some("## Log\n- did a thing".into()),
        checkpoints: vec!["10:00 decision: use X".into()],
        git: vec!["## main...origin/main".into(), " M src/a.rs".into()],
        prompts: vec!["fix the bug\nplease".into()],
        transcript: Some(PathBuf::from("/s/t.jsonl")),
    });
    for part in [
        "# Handoff (written by herdr, 20251013T101500Z)",
        "rev (pane w1:p1)",
        "did not write one",
        "Working directory: /repo",
        "/s/t.jsonl",
        "- fix the bug please",
        " M src/a.rs",
        "- 10:00 decision: use X",
        "- did a thing",
        "## Next step",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    let empty = render_mechanical(&Mechanical::default());
    assert!(empty.contains("(none found)") && empty.contains("(no notes)"));
}

#[test]
fn user_prompts_skip_tool_results_meta_and_herdr_lines() {
    let jsonl = [
        r#"{"type":"user","message":{"role":"user","content":"first"}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#,
        r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#,
        r#"{"type":"user","message":{"content":"<command-name>/compact</command-name>"}}"#,
        r#"{"type":"user","message":{"content":"herdr: your context is 80% full"}}"#,
        r#"{"type":"user","message":{"content":"/compact"}}"#,
        r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued"}}"#,
        r#"{"type":"user","message":{"content":[{"type":"text","text":"second"},{"type":"image"}]}}"#,
        "not json",
        r#"{"type":"user","message":{"content":"<local-command-stdout>done</local-command-stdout>"}}"#,
        r#"{"type":"user","message":{"content":"<div> renders wrong"}}"#,
        r#"{"type":"user","message":{"content":"third"}}"#,
    ]
    .join("\n");
    assert_eq!(
        last_user_prompts(&jsonl, 5),
        vec!["first", "second", "<div> renders wrong", "third"]
    );
    assert_eq!(
        last_user_prompts(&jsonl, 2),
        vec!["<div> renders wrong", "third"]
    );
    assert_eq!(one_line("a\n  b   c", 10), "a b c");
    assert_eq!(one_line("abcdef", 4), "abc\u{2026}");
    assert_eq!(tail_lines("1\n2\n3\n", 2), "2\n3");
}

#[test]
fn the_worker_jobs_copy_point_check_and_write() {
    let base = std::env::temp_dir().join(format!(
        "herdr-agent-handoff-{}-{}",
        std::process::id(),
        crate::codex_sessions::now_unix_ms()
    ));
    let source = base.join("live.jsonl");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        &source,
        "{\"type\":\"user\",\"message\":{\"content\":\"build it\"}}\n",
    )
    .unwrap();
    let paths = run_paths(&root(&base), "s-9", 1_760_350_500);
    let started = files::run_job(files::Job::Start {
        run: 7,
        paths: paths.clone(),
        source: Some(source.clone()),
        locate: None,
        home: None,
        meta: serde_json::json!({ "trigger": "manual" }),
    });
    assert!(matches!(
        started,
        Some(files::Outcome::Started { run: 7, transcript: Ok(n) }) if n > 0
    ));
    assert_eq!(
        std::fs::read_to_string(&paths.transcript).unwrap(),
        std::fs::read_to_string(&source).unwrap()
    );
    assert!(
        !paths.latest.exists(),
        "latest.txt moves only once the run has a handoff"
    );
    assert!(std::fs::read_to_string(&paths.meta)
        .unwrap()
        .contains("\"manual\""));
    let end = Instant::now();
    assert_eq!(
        files::run_job(files::Job::Check {
            run: 7,
            end,
            paths: paths.clone()
        }),
        Some(files::Outcome::Checked {
            run: 7,
            end,
            nonempty: false
        })
    );
    // No transcript: the copy fails, the rest is still set up.
    let other = run_paths(&root(&base), "s-10", 1_760_350_501);
    assert!(matches!(
        files::run_job(files::Job::Start {
            run: 8,
            paths: other.clone(),
            source: None,
            locate: None,
            home: None,
            meta: serde_json::json!({}),
        }),
        Some(files::Outcome::Started {
            transcript: Err(_),
            ..
        })
    ));
    assert!(other.dir.is_dir());
    assert!(!other.latest.exists());
    // The mechanical handoff reads the copy's prompts.
    let written = files::run_job(files::Job::Mechanical {
        run: 7,
        paths: paths.clone(),
        input: Box::new(Mechanical {
            agent_name: "rev".into(),
            pane: "w1:p1".into(),
            trigger: "manual".into(),
            reason: "test".into(),
            written_unix: 1_760_350_500,
            ..Mechanical::default()
        }),
    });
    assert_eq!(
        written,
        Some(files::Outcome::Written {
            run: 7,
            result: Ok(Kind::Mechanical)
        })
    );
    let text = std::fs::read_to_string(&paths.handoff).unwrap();
    assert!(text.contains("- build it"), "{text}");
    assert!(text.contains(&paths.transcript.display().to_string()));
    assert!(files::nonempty(&paths.handoff));
    assert_eq!(
        std::fs::read_to_string(&paths.latest).unwrap().trim(),
        paths.dir.display().to_string(),
        "a handoff moves latest.txt to the run"
    );
    // A handoff the agent wrote is kept, not overwritten by herdr's.
    std::fs::write(&other.handoff, "# mine\n").unwrap();
    assert_eq!(
        files::run_job(files::Job::Mechanical {
            run: 8,
            paths: other.clone(),
            input: Box::new(Mechanical::default()),
        }),
        Some(files::Outcome::Written {
            run: 8,
            result: Ok(Kind::Agent)
        })
    );
    assert_eq!(std::fs::read_to_string(&other.handoff).unwrap(), "# mine\n");
    // A found handoff moves latest.txt too; the copy again refreshes it.
    assert_eq!(
        files::run_job(files::Job::Check {
            run: 8,
            end,
            paths: other.clone()
        }),
        Some(files::Outcome::Checked {
            run: 8,
            end,
            nonempty: true
        })
    );
    assert!(other.latest.is_file());
    std::fs::write(
        &source,
        "{\"type\":\"user\",\"message\":{\"content\":\"more\"}}\n",
    )
    .unwrap();
    assert!(matches!(
        files::run_job(files::Job::Copy {
            run: 7,
            paths: paths.clone(),
            source: Some(source.clone()),
            locate: None,
            home: None,
        }),
        Some(files::Outcome::Started {
            run: 7,
            transcript: Ok(_)
        })
    ));
    assert!(std::fs::read_to_string(&paths.transcript)
        .unwrap()
        .contains("more"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_turn_end_without_the_file_reminds_once_then_falls_back() {
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs(5);
    let mut run = run_at(
        Phase::AwaitHandoff {
            typed_at: t0,
            checked: Some(end),
            checking: false,
        },
        t0,
    );
    let mut f = facts(end + Duration::from_secs(10), true);
    f.hook_end = Some(end);
    assert_eq!(next(&run, &f), Effect::Wait, "no file seen yet");
    run.missing_since = Some(end);
    assert_eq!(next(&run, &f), Effect::Wait, "in the grace");
    f.now = end + MISSING_GRACE;
    assert_eq!(next(&run, &f), Effect::TypeReminder);
    f.free = false;
    assert_eq!(next(&run, &f), Effect::Wait, "only when free");
    // A later turn end is checked first.
    f.hook_end = Some(end + Duration::from_secs(1));
    assert_eq!(
        next(&run, &f),
        Effect::CheckFile {
            end: end + Duration::from_secs(1)
        }
    );
    f.hook_end = Some(end);
    run.reminded = true;
    assert_eq!(
        next(&run, &f),
        Effect::WriteMechanical,
        "after the reminder"
    );
}

#[test]
fn a_compaction_herdr_did_not_start_ends_quietly_without_a_report() {
    let t0 = Instant::now();
    let mut run = run_at(Phase::Compacting { typed_at: t0 }, t0);
    run.trigger = Trigger::External { auto: true };
    assert!(matches!(
        next(&run, &facts(t0 + COMPACT_TIMEOUT, false)),
        Effect::Finish(_)
    ));
}

#[test]
fn each_wait_on_the_worker_ends_in_an_abort_after_the_worker_timeout() {
    let t0 = Instant::now();
    let later = t0 + WORKER_TIMEOUT;
    let before = later - Duration::from_secs(1);

    // The transcript copy before the prompt.
    let mut copying = run_at(Phase::AwaitPrompt, t0);
    copying.transcript_saved = None;
    assert_eq!(next(&copying, &facts(before, true)), Effect::Wait);
    assert!(matches!(
        next(&copying, &facts(later, true)),
        Effect::Abort(reason) if reason.contains("transcript copy")
    ));

    // A file check in flight, counted from when it went out (not from the
    // prompt, nor the turn end it is for).
    let mut checking = run_at(
        Phase::AwaitHandoff {
            typed_at: t0,
            checked: Some(t0),
            checking: true,
        },
        t0,
    );
    let sent = t0 + Duration::from_secs(60);
    checking.check_since = Some(sent);
    assert_eq!(next(&checking, &facts(later, true)), Effect::Wait);
    assert!(matches!(
        next(&checking, &facts(sent + WORKER_TIMEOUT, true)),
        Effect::Abort(reason) if reason.contains("check")
    ));

    // herdr's own handoff: an abort, never another write behind the hung one.
    let writing = run_at(Phase::WritingFallback, t0);
    assert_eq!(next(&writing, &facts(before, true)), Effect::Wait);
    assert!(matches!(
        next(&writing, &facts(later, true)),
        Effect::Abort(reason) if reason.contains("not written in time")
    ));
}

#[cfg(unix)]
#[test]
fn a_git_that_hangs_is_killed_and_gives_no_lines() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!(
        "herdr-agent-handoff-git-{}-{}",
        std::process::id(),
        crate::codex_sessions::now_unix_ms()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let hang = base.join("git-hang");
    std::fs::write(&hang, "#!/bin/sh\necho '## main'\nexec sleep 30\n").unwrap();
    let quick = base.join("git-quick");
    std::fs::write(&quick, "#!/bin/sh\necho '## main'\necho ' M src/lib.rs'\n").unwrap();
    for script in [&hang, &quick] {
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let t0 = Instant::now();
    let lines = files::git_status_with(&hang, &base, Duration::from_millis(300));
    assert!(lines.is_empty(), "{lines:?}");
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    let lines = files::git_status_with(&quick, &base, Duration::from_secs(5));
    assert_eq!(
        lines,
        vec!["## main".to_string(), " M src/lib.rs".to_string()]
    );
    let _ = std::fs::remove_dir_all(&base);
}
