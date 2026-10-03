//! The info dock (fork): layout carve, toggles, the single-slot request
//! queue, replies without notices, keys, text, mouse and the editor.
//! Notes and checkpoints are built from the design fixture (embedded: the
//! stub directory is not part of the repository).

use super::*;
use crate::api::schema::notes::{
    CheckpointContextInfo, CheckpointContextSource, CheckpointInfo, CheckpointKind,
    CheckpointWriteInfo, CheckpointsListInfo, NotesAuthor, NotesInfo, NotesWriteInfo,
    NotesWriteOutcome,
};
use crate::api::schema::{Method, ResponseResult};
use crate::client::shell::info_dock_model::{self as model, InfoDockTarget, InfoView};
use crate::config::Config;
use crate::input::TerminalKey;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

const COLS: u16 = 160;
const ROWS: u16 = 45;

const NOTES: &str = "# calendar-fix\n\nGoal: daily reminders fire at the wrong local time after a DST change.\nIssue: refs #412\n\n## Plan\n- [x] Reproduce with a fixed clock across the Oct 26 transition\n- [x] Find where `daily_time` is converted to an Instant\n- [x] Store the next fire as wall-clock + tz, not a monotonic offset\n- [ ] Migrate persisted `remind_every: daily` tabs\n- [ ] Settings preview shows next fire in local time\n\n## Decisions\n- Use `jiff` zoned arithmetic already in the tree; no new dependency.\n- Missed fires during sleep collapse into one reminder, not a burst.\n\n## Findings\n- `idle_reminders.rs` recomputes from `Instant::now()` on every tick; drift is cumulative.\n- Snapshot round-trip drops the tz name; only the offset survives.\n\n## Open questions\n- Should a skipped 02:30 (spring forward) fire at 03:00 or be skipped? (asked user)\n\n## Agent log\n- 11:52 tests green on macOS; Windows lint pending\n- 12:03 migration draft in `persist/restore.rs`, needs review\n";

const KEY: &str = "claude-6f1c2a9e-4b7d-4e21-9c3a-8d05b2e7f413";
const NOW: u64 = 1_790_943_120;

fn notes_info(text: &str, revision: &str) -> NotesInfo {
    NotesInfo {
        key: KEY.into(),
        path: format!("/tmp/notes/{KEY}.md"),
        revision: revision.into(),
        exists: true,
        unchanged: false,
        text: Some(text.into()),
        bytes: text.len() as u64,
        updated_at: Some(1_790_942_627),
        updated_by: Some(NotesAuthor::Agent),
        agent: Some("claude".into()),
        session_id: None,
        tab_id: Some("tab_1".into()),
        pane_id: Some("pane_1".into()),
        previous: None,
    }
}

fn cp(
    id: &str,
    ts: u64,
    kind: CheckpointKind,
    author: NotesAuthor,
    title: &str,
    detail: Option<&str>,
) -> CheckpointInfo {
    CheckpointInfo {
        id: id.into(),
        ts,
        kind,
        author,
        title: title.into(),
        detail: detail.map(str::to_owned),
        tags: Vec::new(),
        has_context: true,
    }
}

fn fixture_checkpoints() -> Vec<CheckpointInfo> {
    use CheckpointKind::*;
    use NotesAuthor::*;
    vec![
        cp(
            "cp_01JB2K0A1",
            1_790_934_185,
            Note,
            Agent,
            "Session start: reproduce DST reminder drift",
            Some("Scoped to daily reminders; interval reminders are monotonic and unaffected."),
        ),
        cp(
            "cp_01JB2K0A2",
            1_790_935_121,
            Milestone,
            Agent,
            "Reproduced with fixed clock",
            Some("New test `daily_reminder_across_dst` fails: fires 1h early."),
        ),
        cp(
            "cp_01JB2K0A3",
            1_790_935_579,
            Bookmark,
            User,
            "Root cause explanation",
            None,
        ),
        cp(
            "cp_01JB2K0A4",
            1_790_936_043,
            Decision,
            Agent,
            "Store next fire as zoned wall-clock",
            None,
        ),
        cp(
            "cp_01JB2K0A5",
            1_790_937_110,
            Failure,
            Agent,
            "Snapshot round-trip loses tz name",
            Some("session.json keeps only the offset; restored tabs compute in UTC+offset."),
        ),
        cp(
            "cp_01JB2K0A6",
            1_790_937_612,
            Decision,
            Agent,
            "Persist IANA tz name, optional field",
            None,
        ),
        cp(
            "cp_01JB2K0A7",
            1_790_938_653,
            Bookmark,
            Agent,
            "Spring-forward edge case question",
            None,
        ),
        cp(
            "cp_01JB2K0A8",
            1_790_939_528,
            Milestone,
            Agent,
            "DST tests green",
            None,
        ),
        cp(
            "cp_01JB2K0A9",
            1_790_940_404,
            Failure,
            Agent,
            "Windows lint: unused import under cfg(windows)",
            None,
        ),
        cp(
            "cp_01JB2K0AA",
            1_790_941_290,
            Bookmark,
            User,
            "Good summary of the migration options",
            None,
        ),
        cp(
            "cp_01JB2K0AB",
            1_790_941_937,
            Milestone,
            Agent,
            "Tests green on macOS after lint fix",
            None,
        ),
        cp(
            "cp_01JB2K0AC",
            1_790_942_627,
            Decision,
            Agent,
            "Migrate on load (option a)",
            None,
        ),
    ]
}

fn checkpoints_info() -> CheckpointsListInfo {
    CheckpointsListInfo {
        key: KEY.into(),
        seq: 4096,
        unchanged: false,
        checkpoints: fixture_checkpoints(),
        started_at: Some(1_790_934_185),
    }
}

fn context(id: &str, prompt: &str, reply: &str) -> CheckpointContextInfo {
    CheckpointContextInfo {
        id: id.into(),
        source: CheckpointContextSource::Native,
        prompt: Some(prompt.into()),
        reply: Some(reply.into()),
        prompt_ts: None,
        reply_ts: None,
        continued: false,
        truncated: false,
    }
}

// ---------------------------------------------------------------- harness

fn tab(tab_id: &str, label: &str, focused: bool) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: "ws_1".into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused,
        agent_status: AgentStatus::Idle,
        color: None,
        important: false,
        remind_every: None,
    }
}

fn two_tab_snapshot(focused: &str) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.tabs = vec![
        tab("tab_1", "calendar-fix", focused == "tab_1"),
        tab("tab_2", "scratch", focused == "tab_2"),
    ];
    snapshot.focused_tab_id = Some(focused.into());
    snapshot
}

fn state_with(config: Config) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
    state.set_pane_surface(surface());
    state.compose(COLS, ROWS);
    state
}

fn dock_state() -> ClientShellState {
    state_with(Config::default())
}

/// Re-present a surface after a resize invalidated it, and compose.
fn recompose(state: &mut ClientShellState) -> Option<FrameData> {
    if state.pane_surface.is_none() {
        state.set_pane_surface(surface());
    }
    state.compose(COLS, ROWS)
}

fn info_requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

fn info_pending(state: &ClientShellState) -> usize {
    state
        .pending_requests
        .values()
        .filter(|pending| pending.kind.is_info_dock())
        .count()
}

fn tick(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_info_dock(std::time::Instant::now(), &mut outcome);
    outcome
}

fn toggle(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane_for_focused_tab(&mut outcome);
    recompose(state);
    outcome
}

fn answer(
    state: &mut ClientShellState,
    request_id: &str,
    result: Result<ResponseResult, ClientShellEndpointError>,
) -> (bool, Vec<ClientShellAction>) {
    state.handle_endpoint_result("boot-1", request_id, result)
}

fn busy() -> ClientShellEndpointError {
    ClientShellEndpointError {
        code: Some("endpoint_busy".into()),
        message: "busy".into(),
    }
}

fn error(code: &str) -> ClientShellEndpointError {
    ClientShellEndpointError {
        code: Some(code.into()),
        message: code.into(),
    }
}

/// Answer whatever the dock has in flight (one request) with `result`,
/// returning the method it was.
fn answer_in_flight(
    state: &mut ClientShellState,
    outcome: &ClientShellInput,
    result: impl FnOnce(&Method) -> ResponseResult,
) -> Method {
    let requests = info_requests(outcome);
    let [(id, method)] = &requests[..] else {
        panic!("expected one dock request, got {requests:?}");
    };
    answer(state, id, Ok(result(method)));
    method.clone()
}

/// Open the dock with History and Notes loaded.
fn open_loaded(state: &mut ClientShellState) {
    let outcome = toggle(state);
    let method = answer_in_flight(state, &outcome, |_| ResponseResult::CheckpointsList {
        checkpoints: checkpoints_info(),
    });
    assert!(matches!(method, Method::CheckpointsList(_)), "{method:?}");
    state.info_dock.as_deref_mut().expect("dock").view = InfoView::Notes;
    state.info_dock.as_deref_mut().expect("dock").data.notes =
        Some(notes_info(NOTES, "sha256:4e9b1c07a2f3"));
    state
        .info_dock
        .as_deref_mut()
        .expect("dock")
        .data
        .generation += 1;
    recompose(state);
}

fn key(code: KeyCode) -> RawInputEvent {
    RawInputEvent::Key(TerminalKey::new(code, KeyModifiers::empty()))
}

fn click(column: u16, row: u16) -> Vec<RawInputEvent> {
    vec![
        RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }),
        RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }),
    ]
}

fn find_target(state: &ClientShellState, target: &InfoDockTarget) -> (u16, u16) {
    state
        .hits
        .info_dock
        .targets
        .iter()
        .rev()
        .find(|(_, candidate)| candidate == target)
        .map(|(rect, _)| (rect.x, rect.y))
        .unwrap_or_else(|| panic!("no hit for {target:?}"))
}

fn pane_requests(outcome: &ClientShellInput) -> usize {
    outcome
        .requests
        .iter()
        .filter(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. }))
        .count()
}

fn editor_text(state: &ClientShellState) -> String {
    state
        .info_dock
        .as_deref()
        .and_then(|dock| dock.editor.as_ref())
        .map(|editor| editor.lines.join("\n"))
        .expect("editor")
}

// ---------------------------------------------------------------- model

#[test]
fn notes_rows_hide_the_tab_label_heading_and_space_the_others() {
    let rows = model::notes_rows(&model::NotesInput {
        text: NOTES,
        exists: true,
        tab_label: "calendar-fix",
        revision: "sha256:4e9b1c07a2f3",
        updated_by: Some(NotesAuthor::Agent),
        updated_at: Some(1_790_942_627),
        utc_offset: 0,
        width: 42,
    });
    let text: Vec<String> = rows.iter().map(model::Row::text).collect();
    assert!(text[0].starts_with("Goal: daily reminders"), "{text:?}");
    assert!(!text.iter().any(|row| row.contains("calendar-fix")));
    assert!(text.iter().any(|row| row.contains("── ✦ Plan ✦ ──")));
    assert!(text.iter().any(|row| row.starts_with("● Reproduce")));
    assert!(text
        .iter()
        .any(|row| row.starts_with("○ Migrate persisted")));
    assert!(text
        .iter()
        .any(|row| row.starts_with("11:52 · tests green")));
    assert_eq!(
        text.last().map(String::as_str),
        Some("❧ rev 4e9b1c07 · agent · 12:03")
    );
    assert!(text
        .iter()
        .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= 42));

    let other = model::notes_rows(&model::NotesInput {
        text: "# other heading\nbody",
        exists: true,
        tab_label: "calendar-fix",
        revision: "none",
        updated_by: None,
        updated_at: None,
        utc_offset: 0,
        width: 42,
    });
    assert_eq!(other[0].text(), "❦ O T H E R   H E A D I N G");
}

#[test]
fn task_rows_carry_the_revision_they_were_built_from() {
    let rows = model::notes_rows(&model::NotesInput {
        text: NOTES,
        exists: true,
        tab_label: "calendar-fix",
        revision: "sha256:abc",
        updated_by: None,
        updated_at: None,
        utc_offset: 0,
        width: 42,
    });
    let line = NOTES
        .split('\n')
        .position(|line| line.contains("Migrate persisted"))
        .expect("task line");
    assert!(rows.iter().any(|row| row.segs.iter().any(|seg| seg.target
        == Some(InfoDockTarget::Task {
            line,
            revision: "sha256:abc".into(),
        }))));
    let ticked = model::toggle_task_line(NOTES, line).expect("a task");
    assert!(ticked
        .split('\n')
        .nth(line)
        .expect("line")
        .starts_with("- [x] Migrate"));
    assert_eq!(
        model::toggle_task_line(&ticked, line).as_deref(),
        Some(NOTES)
    );
    assert_eq!(
        model::toggle_task_line(NOTES, 0),
        None,
        "a heading is no task"
    );
}

#[test]
fn history_rows_show_chips_spine_and_the_now_row() {
    let checkpoints = fixture_checkpoints();
    let expanded = std::collections::HashSet::from(["cp_01JB2K0A5".to_owned()]);
    let ctx_full = std::collections::HashSet::new();
    let failure_context = context(
        "cp_01JB2K0A5",
        "(continuing)",
        "The fix works live but fails after restore: the persisted remind block only has `offset_minutes`. Restored tabs still drift.",
    );
    let built = model::history_rows(
        &model::HistoryInput {
            checkpoints: &checkpoints,
            filter: None,
            selected: Some("cp_01JB2K0A5"),
            expanded: &expanded,
            ctx_full: &ctx_full,
            agent: "claude",
            now: NOW,
            utc_offset: 0,
            width: 42,
        },
        |checkpoint| {
            if checkpoint.id == "cp_01JB2K0A5" {
                model::ContextView::Loaded(&failure_context)
            } else {
                model::ContextView::Loading
            }
        },
    );
    let chips = built.pinned[0].text();
    assert!(chips.starts_with("◇ 3  ○ 3  × 2  ⚐ 3  · 1"), "{chips:?}");
    assert!(chips.ends_with("all"), "{chips:?}");
    assert!(built.pinned[1].text().ends_with("expand all ▾"));
    let rows: Vec<String> = built.rows.iter().map(model::Row::text).collect();
    assert!(rows[0].starts_with("09:43 · Session start"), "{rows:?}");
    assert!(
        rows.iter().any(|row| row.contains("10:06 ⚑ Root cause")),
        "user bookmarks are filled"
    );
    assert!(rows
        .iter()
        .any(|row| row.contains("(continued from previous turn)")));
    assert!(rows
        .iter()
        .any(|row| row.contains("by claude  #") || row.contains("by claude")));
    assert!(rows.last().expect("now row").contains("12:12 · now"));
    let (first, last) = built.selected.expect("selected span");
    assert!(rows[first].contains("Snapshot round-trip"));
    assert!(last > first + 3, "the open checkpoint spans its body");
}

#[test]
fn long_context_collapses_behind_show_more() {
    let checkpoints = vec![cp(
        "cp_1",
        NOW,
        CheckpointKind::Note,
        NotesAuthor::Agent,
        "t",
        None,
    )];
    let expanded = std::collections::HashSet::from(["cp_1".to_owned()]);
    let long = "word ".repeat(200);
    let ctx = context("cp_1", "prompt", &long);
    let build = |ctx_full: &std::collections::HashSet<String>| {
        model::history_rows(
            &model::HistoryInput {
                checkpoints: &checkpoints,
                filter: None,
                selected: Some("cp_1"),
                expanded: &expanded,
                ctx_full,
                agent: "claude",
                now: NOW,
                utc_offset: 0,
                width: 42,
            },
            |_| model::ContextView::Loaded(&ctx),
        )
        .rows
        .iter()
        .map(model::Row::text)
        .collect::<Vec<_>>()
    };
    let short = build(&std::collections::HashSet::new());
    assert!(
        short.iter().any(|row| row.contains("show more ▾")),
        "{short:?}"
    );
    let full = build(&std::collections::HashSet::from(["cp_1".to_owned()]));
    assert!(full.iter().any(|row| row.contains("show less ▴")));
    assert!(full.len() > short.len());
}

#[test]
fn keep_mine_re_adds_their_new_lines_after_their_anchor() {
    let base = "# t\n## Plan\n- a\n- b";
    let mine: Vec<String> = "# t\n## Plan\n- a\n- b\n- mine"
        .split('\n')
        .map(str::to_owned)
        .collect();
    let theirs = "# t\n## Plan\n- a\n- theirs 1\n- theirs 2\n## Log\n- 12:00 log";
    let (merged, added) = model::keep_mine_merge(base, &mine, theirs);
    assert_eq!(added, 4);
    assert_eq!(
        merged,
        [
            "# t",
            "## Plan",
            "- a",
            "- theirs 1",
            "- theirs 2",
            "## Log",
            "- 12:00 log",
            "- b",
            "- mine"
        ]
    );
    assert_eq!(model::theirs_added(base, &mine, theirs), 4);
    // A line they removed is not removed from mine.
    let (merged, added) = model::keep_mine_merge(base, &mine, "# t\n## Plan\n- a");
    assert_eq!(added, 0);
    assert_eq!(merged, mine);
}

#[test]
fn click_offsets_map_through_spaced_headings_and_code_spans() {
    let line = "Goal: daily reminders";
    assert_eq!(model::edit_offset(line, 6, 3, false), 9);
    let heading = "# notes";
    // "N O T E S": column 4 is "T", the third char.
    assert_eq!(model::edit_offset(heading, 2, 4, true), 4);
    let code = "- Use `jiff` zoned";
    let at = code.find("jiff").expect("code span");
    assert_eq!(model::edit_offset(code, at, 2, false), at + 2);
    let wide = "日本語 text";
    assert_eq!(model::edit_offset(wide, 0, 3, false), "日".len());
}

// ---------------------------------------------------------------- layout

#[test]
fn the_layout_carves_the_dock_from_the_pane_surface() {
    let config = ClientShellConfig::from_config(&Config::default());
    let closed = config.layout(COLS, ROWS, false, 1, 26, None);
    let open = config.layout(COLS, ROWS, false, 1, 26, Some(44));
    assert!(closed.info_dock.is_empty());
    assert_eq!(open.info_dock.width, 44);
    assert_eq!(open.info_dock.right(), closed.pane_surface.right());
    assert_eq!(open.pane_surface.width, closed.pane_surface.width - 44);
    assert_eq!(open.info_dock.height, closed.pane_surface.height);
    // Clamped to the minimum and to 30 columns left for the terminal.
    let tiny = config.layout(COLS, ROWS, false, 1, 26, Some(3));
    assert_eq!(tiny.info_dock.width, model_min());
    let huge = config.layout(COLS, ROWS, false, 1, 26, Some(500));
    assert!(huge.pane_surface.width >= 30);
    assert!(huge.info_dock.width <= COLS * 7 / 10);
    // Too narrow: no carve at all.
    let narrow = config.layout(80, ROWS, false, 1, 26, Some(44));
    let narrow_closed = config.layout(80, ROWS, false, 1, 26, None);
    assert!(narrow.info_dock.is_empty());
    assert_eq!(narrow.pane_surface, narrow_closed.pane_surface);
    // Mobile: never.
    let mobile = config.layout(40, ROWS, false, 1, 26, Some(44));
    assert!(mobile.info_dock.is_empty());
}

fn model_min() -> u16 {
    super::super::info_dock::DOCK_MIN
}

mod fork_smoke {
    use super::*;

    #[test]
    fn dock_carves_the_surface_and_resizes_per_tab() {
        let mut state = dock_state();
        let before = state.surface_size(COLS, ROWS);
        assert!(state.info_dock.is_none(), "lazy until the first toggle");
        let outcome = toggle(&mut state);
        assert!(outcome.resize);
        let open = state.surface_size(COLS, ROWS);
        assert_eq!(open.cols, before.cols - 44);
        assert_eq!(open.rows, before.rows);
        // Another tab without a dock gets the full width back.
        state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
        assert_eq!(state.surface_size(COLS, ROWS), before);
        state.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
        assert_eq!(state.surface_size(COLS, ROWS), open);
        let frame = recompose(&mut state).expect("frame");
        let rows = frame_rows(&frame);
        assert!(
            rows.iter().any(|row| row.contains("✦ History ✦")),
            "{rows:?}"
        );
        toggle(&mut state);
        assert_eq!(state.surface_size(COLS, ROWS), before);
    }
}

#[test]
fn a_too_narrow_window_keeps_the_dock_open_and_says_so() {
    let mut state = dock_state();
    state.compose(70, ROWS);
    let before = state.surface_size(70, ROWS);
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane_for_focused_tab(&mut outcome);
    assert!(!outcome.resize);
    assert_eq!(state.surface_size(70, ROWS), before);
    assert!(state.info_dock_open_for_focused_tab());
    let notice = state.visible_endpoint_notice.as_ref().expect("a notice");
    assert_eq!(notice.title, "Info pane");
    assert!(state.info_dock_area().is_empty());
}

#[test]
fn the_width_survives_a_preferences_round_trip() {
    let path = std::env::temp_dir().join(format!(
        "herdr-info-dock-preferences-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
    state.set_pane_surface(surface());
    state.compose(COLS, ROWS);
    toggle(&mut state);
    let mut outcome = ClientShellInput::default();
    state.handle_info_dock_key(
        &TerminalKey::new(KeyCode::Char('>'), KeyModifiers::empty()),
        &mut outcome,
    );
    assert_eq!(state.info_dock_width, 48);
    let saved = preferences::load(&path).expect("saved");
    assert_eq!(saved.info_dock_width, Some(48));
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let restored = ClientShellState::new(config);
    assert_eq!(restored.info_dock_width, 48);
    assert!(restored.info_dock_width_manual);
    std::fs::remove_file(path).expect("remove preferences");
}

#[test]
fn width_keys_clamp() {
    let mut state = dock_state();
    toggle(&mut state);
    let mut outcome = ClientShellInput::default();
    for _ in 0..10 {
        state.handle_info_dock_key(
            &TerminalKey::new(KeyCode::Char('<'), KeyModifiers::empty()),
            &mut outcome,
        );
        recompose(&mut state);
    }
    assert_eq!(state.info_dock_area().width, model_min());
    for _ in 0..60 {
        state.handle_info_dock_key(
            &TerminalKey::new(KeyCode::Char('>'), KeyModifiers::empty()),
            &mut outcome,
        );
        recompose(&mut state);
    }
    let layout = state.layout(COLS, ROWS);
    assert!(layout.pane_surface.width >= 30);
    assert!(layout.info_dock.width <= COLS * 7 / 10);
}

#[test]
fn dragging_the_divider_resizes_and_a_double_click_resets() {
    let mut state = dock_state();
    toggle(&mut state);
    let divider = state.hits.info_dock.divider;
    assert_eq!(divider.x, COLS - 44);
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: divider.x,
        row: 5,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.info_dock.as_deref().expect("dock").dragging);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 100,
        row: 5,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(outcome.resize);
    assert_eq!(state.info_dock_width, 60);
    assert!(state.info_dock_width_manual);
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 100,
        row: 5,
        modifiers: KeyModifiers::empty(),
    })]);
    let dock = state.info_dock.as_deref().expect("dock");
    assert!(!dock.dragging);
    recompose(&mut state);
    let divider = state.hits.info_dock.divider;
    assert_eq!(divider.x, COLS - 60);
    state.handle_raw_events(click(divider.x, 5));
    state.handle_raw_events(click(divider.x, 5));
    assert_eq!(state.info_dock_width, 44);
    assert!(!state.info_dock_width_manual);
}

// ---------------------------------------------------------------- input

#[test]
fn a_dock_click_never_starts_a_pane_gesture_and_the_wheel_is_the_docks() {
    let mut state = dock_state();
    open_loaded(&mut state);
    let body = state.hits.info_dock.body;
    let outcome = state.handle_raw_events(click(body.x + 5, body.y + 3));
    assert!(state.pane_mouse_gesture.is_none());
    assert!(state.selection.is_none());
    assert_eq!(pane_requests(&outcome), 0);
    assert!(state.info_dock.as_deref().expect("dock").focused);
    state.info_dock.as_deref_mut().expect("dock").view = InfoView::Notes;
    recompose(&mut state);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: body.x + 5,
        row: body.y + 3,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(pane_requests(&outcome), 0);
    assert!(info_requests(&outcome)
        .iter()
        .all(|(_, method)| !matches!(method, Method::PaneScroll(_))));
}

#[test]
fn keys_go_to_the_dock_only_while_it_has_focus() {
    let mut state = dock_state();
    open_loaded(&mut state);
    state.info_dock.as_deref_mut().expect("dock").view = InfoView::History;
    recompose(&mut state);
    state.info_dock.as_deref_mut().expect("dock").focused = true;
    let outcome = state.handle_raw_events(vec![key(KeyCode::Char('k'))]);
    assert_eq!(pane_requests(&outcome), 0, "k moves the selection");
    let selected = state
        .info_dock
        .as_deref_mut()
        .and_then(|dock| dock.ui.get(KEY).and_then(|ui| ui.selected.clone()));
    assert_eq!(selected.as_deref(), Some("cp_01JB2K0AB"));
    // Esc hands the keys back to the pane; the dock stays open.
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(!state.info_dock_focused());
    assert!(state.info_dock_open_for_focused_tab());
    let outcome = state.handle_raw_events(vec![key(KeyCode::Char('k'))]);
    assert_eq!(pane_requests(&outcome), 1);
}

#[test]
fn while_editing_tab_digits_and_angles_are_text_and_paste_stays_in_the_dock() {
    let mut state = dock_state();
    open_loaded(&mut state);
    state.info_dock.as_deref_mut().expect("dock").focused = true;
    state.handle_raw_events(vec![key(KeyCode::Char('e'))]);
    assert!(state.info_dock_editing());
    let outcome = state.handle_raw_events(vec![
        key(KeyCode::Tab),
        key(KeyCode::Char('1')),
        key(KeyCode::Char('<')),
        RawInputEvent::Paste("pasted".into()),
    ]);
    assert_eq!(pane_requests(&outcome), 0);
    assert_eq!(state.info_dock_width, 44, "< is text while editing");
    let text = editor_text(&state);
    assert!(text.contains("needs review    1<pasted"), "{text:?}");
    assert_eq!(
        state.info_dock.as_deref().expect("dock").view,
        InfoView::Notes
    );
}

#[test]
fn clicking_text_edits_at_the_clicked_column() {
    let mut state = dock_state();
    open_loaded(&mut state);
    let goal = NOTES
        .split('\n')
        .position(|line| line.starts_with("Goal:"))
        .expect("goal line");
    let target = InfoDockTarget::EditAt {
        line: goal,
        byte_start: "Goal: ".len(),
        spaced: false,
    };
    let (x, y) = find_target(&state, &target);
    state.handle_raw_events(click(x + 3, y));
    let dock = state.info_dock.as_deref().expect("dock");
    let editor = dock.editor.as_ref().expect("editing");
    assert_eq!(editor.cursor, (goal, "Goal: ".len() + 3));
    assert!(dock.focused);
}

// ---------------------------------------------------------------- saving

fn start_dirty_edit(state: &mut ClientShellState) {
    open_loaded(state);
    state.info_dock.as_deref_mut().expect("dock").focused = true;
    state.handle_raw_events(vec![key(KeyCode::Char('e')), key(KeyCode::Char('!'))]);
    assert!(state
        .info_dock
        .as_deref()
        .and_then(|dock| dock.editor.as_ref())
        .is_some_and(|editor| editor.dirty));
}

fn sent_save(
    outcome: &ClientShellInput,
) -> Option<(String, crate::api::schema::notes::NotesSetParams)> {
    info_requests(outcome)
        .into_iter()
        .find_map(|(id, method)| match method {
            Method::NotesSet(params) => Some((id, params)),
            _ => None,
        })
}

#[test]
fn the_editor_saves_on_a_view_click() {
    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let (x, y) = find_target(&state, &InfoDockTarget::View(InfoView::History));
    let outcome = state.handle_raw_events(click(x, y));
    let (_, params) = sent_save(&outcome).expect("a save");
    assert!(params.text.ends_with("needs review!\n") || params.text.contains("review!"));
    assert_eq!(params.base_revision.as_deref(), Some("sha256:4e9b1c07a2f3"));
    assert_eq!(params.target.key.as_deref(), Some(KEY));
    assert_eq!(params.author, NotesAuthor::User);
}

#[test]
fn the_editor_saves_when_the_dock_closes_and_on_a_pane_press() {
    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane_for_focused_tab(&mut outcome);
    assert!(sent_save(&outcome).is_some(), "closing sends the save");

    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let pane = state.hits.panes[0].rect;
    let outcome = state.handle_raw_events(click(pane.x + 1, pane.y));
    assert!(sent_save(&outcome).is_some(), "a pane press sends the save");
    assert!(!state.info_dock.as_deref().expect("dock").focused);
}

#[test]
fn a_task_tick_is_a_compare_and_swap_and_a_stale_row_is_refused() {
    let mut state = dock_state();
    open_loaded(&mut state);
    let line = NOTES
        .split('\n')
        .position(|line| line.contains("Migrate persisted"))
        .expect("task");
    let target = InfoDockTarget::Task {
        line,
        revision: "sha256:4e9b1c07a2f3".into(),
    };
    let (x, y) = find_target(&state, &target);
    let outcome = state.handle_raw_events(click(x, y));
    let (_, params) = sent_save(&outcome).expect("a tick save");
    assert_eq!(params.base_revision.as_deref(), Some("sha256:4e9b1c07a2f3"));
    assert!(params.text.contains("- [x] Migrate persisted"));
    assert!(state.info_dock.as_deref().expect("dock").editor.is_none());

    let mut state = dock_state();
    open_loaded(&mut state);
    let mut outcome = ClientShellInput::default();
    // The row was drawn from an older revision.
    let stale = InfoDockTarget::Task {
        line,
        revision: "sha256:old".into(),
    };
    state.info_dock_activate_for_test(stale, &mut outcome);
    assert!(sent_save(&outcome).is_none());
    assert_eq!(
        state
            .info_dock
            .as_deref()
            .and_then(|dock| dock.flash.as_ref())
            .map(|(text, _)| text.as_str()),
        Some("notes changed, tick again")
    );
}

#[test]
fn no_notes_get_is_polled_while_editing() {
    let mut state = dock_state();
    start_dirty_edit(&mut state);
    for step in 0..4 {
        let mut outcome = ClientShellInput::default();
        state.tick_info_dock(
            std::time::Instant::now() + std::time::Duration::from_secs(6 * (step + 1)),
            &mut outcome,
        );
        for (id, method) in info_requests(&outcome) {
            assert!(!matches!(method, Method::NotesGet(_)), "{method:?}");
            answer(
                &mut state,
                &id,
                Ok(ResponseResult::NotesWrite {
                    write: NotesWriteInfo {
                        outcome: NotesWriteOutcome::Written,
                        notes: notes_info(NOTES, "sha256:new"),
                    },
                }),
            );
        }
    }
}

#[test]
fn a_conflict_offers_reload_and_keep_mine() {
    let mut state = dock_state();
    start_dirty_edit(&mut state);
    // Esc saves and leaves the editor.
    let outcome = state.handle_raw_events(vec![key(KeyCode::Esc)]);
    let save = sent_save(&outcome);
    let (id, _) = save.expect("save on Esc");
    let theirs = format!("{NOTES}- 12:30 appended by the agent");
    answer(
        &mut state,
        &id,
        Ok(ResponseResult::NotesWrite {
            write: NotesWriteInfo {
                outcome: NotesWriteOutcome::Conflict,
                notes: notes_info(&theirs, "sha256:theirs"),
            },
        }),
    );
    recompose(&mut state);
    let (x, y) = find_target(&state, &InfoDockTarget::ConflictKeepMine);
    let outcome = state.handle_raw_events(click(x, y));
    let (_, params) = sent_save(&outcome).expect("keep mine re-sends");
    assert_eq!(params.base_revision.as_deref(), Some("sha256:theirs"));
    assert!(params.text.contains("appended by the agent"));
    assert!(params.text.contains("review!"));

    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane_for_focused_tab(&mut outcome);
    let (id, _) = sent_save(&outcome).expect("save on close");
    answer(
        &mut state,
        &id,
        Ok(ResponseResult::NotesWrite {
            write: NotesWriteInfo {
                outcome: NotesWriteOutcome::Conflict,
                notes: notes_info(&theirs, "sha256:theirs"),
            },
        }),
    );
    let dock = state.info_dock.as_deref_mut().expect("dock");
    assert!(dock
        .editor
        .as_ref()
        .is_some_and(|editor| editor.conflict.is_some()));
    let mut outcome = ClientShellInput::default();
    state.info_dock_activate_for_test(InfoDockTarget::ConflictReload, &mut outcome);
    assert_eq!(editor_text(&state), theirs);
}

// ---------------------------------------------------------------- replies

#[test]
fn busy_replies_raise_no_notice_retry_and_keep_a_dirty_save() {
    let mut state = dock_state();
    let outcome = toggle(&mut state);
    let requests = info_requests(&outcome);
    let [(id, Method::CheckpointsList(_))] = &requests[..] else {
        panic!("{requests:?}");
    };
    let (repaint, actions) = answer(&mut state, id, Err(busy()));
    assert!(repaint);
    assert!(actions.is_empty(), "the retry waits");
    assert!(state.visible_endpoint_notice.is_none());
    assert!(state
        .info_dock
        .as_deref()
        .expect("dock")
        .data
        .in_flight
        .is_none());
    let mut later = ClientShellInput::default();
    state.tick_info_dock(
        std::time::Instant::now() + std::time::Duration::from_millis(300),
        &mut later,
    );
    assert!(matches!(
        &info_requests(&later)[..],
        [(_, Method::CheckpointsList(_))]
    ));

    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane_for_focused_tab(&mut outcome);
    let (id, _) = sent_save(&outcome).expect("save");
    answer(&mut state, &id, Err(busy()));
    assert!(state.visible_endpoint_notice.is_none());
    let editor = state
        .info_dock
        .as_deref()
        .and_then(|dock| dock.editor.as_ref())
        .expect("buffer kept");
    assert!(editor.dirty && editor.save_pending);
    let mut later = ClientShellInput::default();
    state.tick_info_dock(
        std::time::Instant::now() + std::time::Duration::from_millis(300),
        &mut later,
    );
    assert!(
        sent_save(&later).is_some(),
        "a closed dock still sends its save"
    );
}

#[test]
fn dock_errors_never_toast() {
    for code in [
        "notes_disabled",
        "not_found",
        "too_large",
        "rate_limited",
        "invalid_params",
    ] {
        let mut state = dock_state();
        let outcome = toggle(&mut state);
        let requests = info_requests(&outcome);
        answer(&mut state, &requests[0].0, Err(error(code)));
        assert!(state.visible_endpoint_notice.is_none(), "{code}");
    }
    let mut state = dock_state();
    let outcome = toggle(&mut state);
    let requests = info_requests(&outcome);
    answer(&mut state, &requests[0].0, Err(error("notes_disabled")));
    let frame = recompose(&mut state).expect("frame");
    assert!(frame_rows(&frame)
        .iter()
        .any(|row| row.contains("notes are turned off")));
}

#[test]
fn only_one_dock_request_is_ever_in_flight() {
    let mut state = dock_state();
    let outcome = toggle(&mut state);
    assert_eq!(info_requests(&outcome).len(), 1);
    let dock = state.info_dock.as_deref_mut().expect("dock");
    dock.queue(super::super::info_dock::InfoRequest::NotesGet, None);
    dock.queue(super::super::info_dock::InfoRequest::BookmarkAdd, None);
    for _ in 0..5 {
        let outcome = tick(&mut state);
        assert!(info_requests(&outcome).is_empty());
        assert_eq!(info_pending(&state), 1);
    }
}

#[test]
fn the_dock_waits_while_another_command_is_pending() {
    let mut state = dock_state();
    let mut outcome = ClientShellInput::default();
    state.push_endpoint_method(
        Method::TabFocus(crate::api::schema::TabTarget {
            tab_id: "tab_2".into(),
        }),
        &mut outcome,
    );
    let outcome = toggle(&mut state);
    assert!(info_requests(&outcome).is_empty());
    let (id, _) = state
        .pending_requests
        .iter()
        .next()
        .map(|(id, pending)| (id.clone(), pending.method_name.clone()))
        .expect("tab focus pending");
    answer(&mut state, &id, Ok(ResponseResult::Ok {}));
    let outcome = tick(&mut state);
    assert_eq!(info_requests(&outcome).len(), 1);
}

#[test]
fn a_reboot_clears_the_request_in_flight() {
    let mut state = dock_state();
    toggle(&mut state);
    assert_eq!(info_pending(&state), 1);
    let mut rebooted = two_tab_snapshot("tab_1");
    rebooted.boot_id = "boot-2".into();
    state.set_snapshot(Box::new(rebooted));
    let mut surface = surface();
    surface.boot_id = "boot-2".into();
    state.set_pane_surface(surface);
    state.compose(COLS, ROWS);
    let outcome = tick(&mut state);
    assert!(state
        .info_dock
        .as_deref()
        .expect("dock")
        .data
        .in_flight
        .is_some());
    assert_eq!(
        info_requests(&outcome).len(),
        1,
        "a fresh pull on the new server"
    );
}

#[test]
fn an_unsupported_server_says_so_in_the_body() {
    let mut state = dock_state();
    state.set_endpoint_methods(Some(vec!["tab.focus".into()]));
    let outcome = toggle(&mut state);
    assert!(info_requests(&outcome).is_empty());
    assert!(state.visible_endpoint_notice.is_none());
    let frame = recompose(&mut state).expect("frame");
    assert!(frame_rows(&frame)
        .iter()
        .any(|row| row.contains("not available on this server")));
}

// ---------------------------------------------------------------- stub semantics

#[test]
fn stub_semantics_hold() {
    let mut state = dock_state();
    open_loaded(&mut state);
    let mut outcome = ClientShellInput::default();
    // 2 follows, 1 does not.
    state.info_dock.as_deref_mut().expect("dock").focused = true;
    state.handle_raw_events(vec![key(KeyCode::Char('2'))]);
    {
        let dock = state.info_dock.as_deref_mut().expect("dock");
        assert_eq!(dock.view, InfoView::History);
        assert!(dock.ui.get(KEY).is_some_and(|ui| ui.follow));
    }
    recompose(&mut state);
    // Expand all covers only the filtered set.
    state.info_dock_activate_for_test(
        InfoDockTarget::Filter(Some(CheckpointKind::Failure)),
        &mut outcome,
    );
    state.info_dock_activate_for_test(InfoDockTarget::ExpandAll, &mut outcome);
    {
        let dock = state.info_dock.as_deref_mut().expect("dock");
        let ui = dock.ui.get(KEY).expect("ui");
        assert_eq!(ui.expanded.len(), 2);
        assert!(ui.expanded.contains("cp_01JB2K0A5"));
    }
    // A bookmark resets a non-bookmark filter.
    state.handle_raw_events(vec![key(KeyCode::Char('b'))]);
    {
        let dock = state.info_dock.as_deref_mut().expect("dock");
        assert_eq!(dock.ui.get(KEY).expect("ui").filter, None);
        assert!(dock.flash.is_some());
    }
    recompose(&mut state);
    // A dock press clears the flash; show more keeps follow off.
    let body = state.hits.info_dock.body;
    state.handle_raw_events(click(body.x + 4, body.y + 1));
    assert!(state.info_dock.as_deref().expect("dock").flash.is_none());
    recompose(&mut state);
    state.info_dock_activate_for_test(
        InfoDockTarget::ContextMore("cp_01JB2K0A5".into()),
        &mut outcome,
    );
    let dock = state.info_dock.as_deref_mut().expect("dock");
    let ui = dock.ui.get(KEY).expect("ui");
    assert!(ui.ctx_full.contains("cp_01JB2K0A5"));
    assert!(!ui.follow);
}

#[test]
fn a_closed_dock_costs_nothing() {
    let mut state = dock_state();
    assert_eq!(
        state.next_info_dock_deadline(std::time::Instant::now()),
        None
    );
    toggle(&mut state);
    toggle(&mut state);
    // The pull sent on open may still be answered; nothing new goes out.
    let pending: Vec<String> = state.pending_requests.keys().cloned().collect();
    for id in pending {
        answer(
            &mut state,
            &id,
            Ok(ResponseResult::CheckpointsList {
                checkpoints: checkpoints_info(),
            }),
        );
    }
    assert_eq!(
        state.next_info_dock_deadline(std::time::Instant::now()),
        None
    );
    for step in 0..20 {
        let mut outcome = ClientShellInput::default();
        state.tick_info_dock(
            std::time::Instant::now() + std::time::Duration::from_secs(step * 3),
            &mut outcome,
        );
        assert!(info_requests(&outcome).is_empty());
    }
    assert_eq!(info_pending(&state), 0);
}

#[test]
fn contexts_are_fetched_once_when_a_row_opens() {
    let mut state = dock_state();
    open_loaded(&mut state);
    state.info_dock.as_deref_mut().expect("dock").view = InfoView::History;
    let mut outcome = ClientShellInput::default();
    state.info_dock_activate_for_test(InfoDockTarget::Toggle("cp_01JB2K0A2".into()), &mut outcome);
    let requests = info_requests(&outcome);
    let [(id, Method::CheckpointsContext(params))] = &requests[..] else {
        panic!("{requests:?}");
    };
    assert_eq!(params.id, "cp_01JB2K0A2");
    assert_eq!(params.target.key.as_deref(), Some(KEY));
    // Pending: asked again shortly.
    let mut pending = context("cp_01JB2K0A2", "", "");
    pending.source = CheckpointContextSource::Pending;
    answer(
        &mut state,
        id,
        Ok(ResponseResult::CheckpointContext { context: pending }),
    );
    let mut later = ClientShellInput::default();
    state.tick_info_dock(
        std::time::Instant::now() + std::time::Duration::from_millis(600),
        &mut later,
    );
    let requests = info_requests(&later);
    let [(id, Method::CheckpointsContext(_))] = &requests[..] else {
        panic!("{requests:?}");
    };
    answer(
        &mut state,
        id,
        Ok(ResponseResult::CheckpointContext {
            context: context("cp_01JB2K0A2", "ok go ahead", "Reproduced."),
        }),
    );
    // Closing and reopening the row asks nothing more.
    let mut outcome = ClientShellInput::default();
    state.info_dock_activate_for_test(InfoDockTarget::Toggle("cp_01JB2K0A2".into()), &mut outcome);
    state.info_dock_activate_for_test(InfoDockTarget::Toggle("cp_01JB2K0A2".into()), &mut outcome);
    assert!(info_requests(&outcome).is_empty());
    let frame = recompose(&mut state).expect("frame");
    assert!(frame_rows(&frame)
        .iter()
        .any(|row| row.contains("ok go ahead")));
}

#[test]
fn bookmark_written_selects_the_new_checkpoint() {
    let mut state = dock_state();
    open_loaded(&mut state);
    let dock = state.info_dock.as_deref_mut().expect("dock");
    dock.view = InfoView::History;
    dock.focused = true;
    recompose(&mut state);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Char('b'))]);
    let requests = info_requests(&outcome);
    let [(id, Method::CheckpointsAdd(params))] = &requests[..] else {
        panic!("{requests:?}");
    };
    assert_eq!(params.kind, CheckpointKind::Bookmark);
    assert_eq!(params.author, NotesAuthor::User);
    let added = cp(
        "cp_new",
        NOW,
        CheckpointKind::Bookmark,
        NotesAuthor::User,
        "Pinned from the pane",
        None,
    );
    let (_, actions) = answer(
        &mut state,
        id,
        Ok(ResponseResult::CheckpointWrite {
            checkpoint: CheckpointWriteInfo {
                key: KEY.into(),
                seq: 5000,
                checkpoint: Some(added),
                folded: false,
                removed: false,
            },
        }),
    );
    let dock = state.info_dock.as_deref_mut().expect("dock");
    assert_eq!(
        dock.ui
            .get(KEY)
            .and_then(|ui| ui.selected.clone())
            .as_deref(),
        Some("cp_new")
    );
    let next = ClientShellInput {
        actions,
        ..ClientShellInput::default()
    };
    assert!(matches!(
        &info_requests(&next)[..],
        [(_, Method::CheckpointsList(_))]
    ));
}

#[test]
fn the_pane_menu_toggles_the_tabs_info_pane() {
    let mut state = dock_state();
    state.open_pane_context_menu("pane_1".into(), 40, 5);
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("pane menu");
    };
    let index = menu
        .items()
        .iter()
        .position(|item| item.action == ClientContextMenuAction::ToggleInfoPane)
        .expect("Info pane item");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(state.info_dock_open_for_focused_tab());
    assert!(outcome.resize);
}

#[test]
fn the_tab_menu_toggles_that_tabs_info_pane_without_focusing_it() {
    fn pick_info_pane(state: &mut ClientShellState, tab_id: &str) -> ClientShellInput {
        state.open_tab_context_menu(tab_id.into(), 40, 5);
        let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
            panic!("tab menu");
        };
        let items = menu.items();
        let index = items
            .iter()
            .position(|item| item.action == ClientContextMenuAction::ToggleInfoPane)
            .expect("Info pane item");
        assert_eq!(items[index].label, "Info pane");
        let close = items
            .iter()
            .position(|item| item.action == ClientContextMenuAction::Close)
            .expect("close");
        let important = items
            .iter()
            .position(|item| item.action == ClientContextMenuAction::Important)
            .expect("important");
        assert!(
            close < index && index < important,
            "between Close and Important"
        );
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(index, &mut outcome);
        assert!(
            !info_requests(&outcome)
                .iter()
                .any(|(_, method)| matches!(method, Method::TabFocus(_))),
            "the tab is not focused"
        );
        outcome
    }

    // The focused tab: the dock opens and the surface narrows.
    let mut state = dock_state();
    let outcome = pick_info_pane(&mut state, "tab_1");
    assert!(state.info_dock_open_for_focused_tab());
    assert!(outcome.resize);

    // A background tab: state only, nothing focused or resized.
    let mut state = dock_state();
    let outcome = pick_info_pane(&mut state, "tab_2");
    assert!(!state.info_dock_open_for_focused_tab());
    assert!(!outcome.resize);
    let key = (ClientEndpointId::Local, "tab_2".to_owned());
    assert!(state
        .info_dock
        .as_deref()
        .is_some_and(|dock| dock.open_tabs.contains(&key)));
    // Again closes it.
    pick_info_pane(&mut state, "tab_2");
    assert!(!state
        .info_dock
        .as_deref()
        .is_some_and(|dock| dock.open_tabs.contains(&key)));
}

#[test]
fn the_dock_deadline_is_never_in_the_past() {
    let mut state = dock_state();
    // Another command holds the slot, so the dock's pull waits in the queue.
    let mut outcome = ClientShellInput::default();
    state.push_endpoint_method(
        Method::TabFocus(crate::api::schema::TabTarget {
            tab_id: "tab_2".into(),
        }),
        &mut outcome,
    );
    toggle(&mut state);
    let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let deadline = state.next_info_dock_deadline(later);
    assert!(deadline.is_none_or(|at| at > later), "{deadline:?}");
    assert!(state.timer_delay(later) > std::time::Duration::ZERO);
}
