//! The info dock (fork): layout carve, toggles, the single-slot request
//! queue, replies without notices, keys, text, mouse, the editor and the
//! notes page's markdown rows (the picked design,
//! `.local/prd/notes-format/sonnet-directed.html`).

use super::*;
use crate::api::schema::notes::{NotesAuthor, NotesInfo, NotesWriteInfo, NotesWriteOutcome};
use crate::api::schema::{Method, ResponseResult};
use crate::client::shell::info_dock_model::{self as model, InfoDockTarget};
use crate::config::Config;
use crate::input::TerminalKey;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

const COLS: u16 = 160;
const ROWS: u16 = 45;

const NOTES: &str = "# calendar-fix\n\nGoal: daily reminders fire at the wrong local time after a DST change.\nIssue: refs #412\n\n## Plan\n- [x] Reproduce with a fixed clock across the Oct 26 transition\n- [x] Find where `daily_time` is converted to an Instant\n- [x] Store the next fire as wall-clock + tz, not a monotonic offset\n- [ ] Migrate persisted `remind_every: daily` tabs\n- [ ] Settings preview shows next fire in local time\n\n## Decisions\n- Use `jiff` zoned arithmetic already in the tree; no new dependency.\n- Missed fires during sleep collapse into one reminder, not a burst.\n\n## Findings\n- `idle_reminders.rs` recomputes from `Instant::now()` on every tick; drift is cumulative.\n- Snapshot round-trip drops the tz name; only the offset survives.\n\n## Open questions\n- Should a skipped 02:30 (spring forward) fire at 03:00 or be skipped? (asked user)\n\n## Agent log\n- 11:52 tests green on macOS; Windows lint pending\n- 12:03 migration draft in `persist/restore.rs`, needs review\n";

const KEY: &str = "claude-6f1c2a9e-4b7d-4e21-9c3a-8d05b2e7f413";

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
    state.compose(COLS, ROWS).map(|composed| composed.frame)
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

/// Open the dock with the notes loaded.
fn open_loaded(state: &mut ClientShellState) {
    let outcome = toggle(state);
    let method = answer_in_flight(state, &outcome, |_| ResponseResult::NotesGet {
        notes: notes_info(NOTES, "sha256:4e9b1c07a2f3"),
    });
    assert!(matches!(method, Method::NotesGet(_)), "{method:?}");
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

// ---------------------------------------------------------------- markdown

/// The design brief's stub (`.local/prd/notes-format/brief.md`): every
/// element the pane renders.
const BRIEF_STUB: &str = "# Backup S3 watch\n\nOwner: infra monitor (w12:p1), daily since 2026-10-31.\nFlags go to the owner in **Turkish**, see [the /tr skill](https://example.com/tr).\n\n## Daily checklist\n\n### Cost\n- [ ] Run `check.sh`, compare with *yesterday's* line\n- [x] S3 cost under $1.30/day\n\n### Buckets\n1. New bucket: flag one day above +15 GB\n2. Old bucket: flag growth above 5 GB/day\n   - report its size once on 11-07\n   - then stop\n\n#### Known failed jobs\n- 29853085\n- 29864525\n\n> Nightly full passes must succeed within 48 h.\n\n```bash\nssh -p 10041 root@master 'kubectl get jobs -n backup'\n```\n\n---\n\n## Reference\n| Bucket | Purpose | Expiry |\n|---|---|---|\n| `leap-bi4-backups` | new | none |\n| `leap-bi4-s3-access-logs` | logs | 14 d |\n\n## Log\n- 10:30 egress fix live (`167fdd49`)\n- 11:05 handed to infra monitor\n";

fn md_rows(text: &str, width: usize) -> Vec<model::Row> {
    model::notes_rows(&model::NotesInput {
        text,
        exists: true,
        tab_label: "",
        revision: "sha256:abc",
        updated_by: None,
        updated_at: None,
        utc_offset: 0,
        width,
    })
}

fn md_text(text: &str, width: usize) -> Vec<String> {
    md_rows(text, width).iter().map(model::Row::text).collect()
}

/// The segment drawing `text` in the first row that has one.
fn seg_of<'a>(rows: &'a [model::Row], text: &str) -> &'a model::Seg {
    rows.iter()
        .flat_map(|row| row.segs.iter())
        .find(|seg| seg.text == text)
        .unwrap_or_else(|| panic!("no segment {text:?}"))
}

fn has(style: ratatui::style::Style, modifier: ratatui::style::Modifier) -> bool {
    style.add_modifier.contains(modifier)
}

fn rule_row(label: &str, width: usize) -> String {
    let head = format!("── ✦ {label} ✦ ");
    let used = unicode_width::UnicodeWidthStr::width(head.as_str());
    format!("{head}{}", "─".repeat(width - used))
}

#[test]
fn the_brief_stub_renders_as_the_picked_design() {
    use ratatui::style::Modifier;
    let pad = |text: &str| format!("{text}{}", " ".repeat(56 - text.chars().count()));
    let rows = md_rows(BRIEF_STUB, 56);
    let text: Vec<String> = rows.iter().map(model::Row::text).collect();
    let expected = [
        "❦ B A C K U P   S 3   W A T C H".to_owned(),
        String::new(),
        "Owner: infra monitor (w12:p1), daily since 2026-10-31.".to_owned(),
        "Flags go to the owner in Turkish, see the /tr skill.".to_owned(),
        String::new(),
        rule_row("Daily checklist", 56),
        String::new(),
        "Cost".to_owned(),
        "○ Run check.sh, compare with yesterday's line".to_owned(),
        "● S3 cost under $1.30/day".to_owned(),
        String::new(),
        "Buckets".to_owned(),
        "1. New bucket: flag one day above +15 GB".to_owned(),
        "2. Old bucket: flag growth above 5 GB/day".to_owned(),
        "   · report its size once on 11-07".to_owned(),
        "   · then stop".to_owned(),
        String::new(),
        "  Known failed jobs".to_owned(),
        "· 29853085".to_owned(),
        "· 29864525".to_owned(),
        String::new(),
        "  ▎ Nightly full passes must succeed within 48 h.".to_owned(),
        String::new(),
        pad("  ▎ ssh -p 10041 root@master 'kubectl get jobs -n"),
        pad("  ▎   backup'"),
        String::new(),
        "─".repeat(56),
        String::new(),
        rule_row("Reference", 56),
        String::new(),
        "Bucket                  │ Purpose │ Expiry".to_owned(),
        "────────────────────────┼─────────┼───────".to_owned(),
        "leap-bi4-backups        │ new     │ none".to_owned(),
        "leap-bi4-s3-access-logs │ logs    │ 14 d".to_owned(),
        String::new(),
        rule_row("Log", 56),
        String::new(),
        "10:30 · egress fix live (167fdd49)".to_owned(),
        "11:05 · handed to infra monitor".to_owned(),
        String::new(),
        String::new(),
        "❧ rev abc".to_owned(),
    ];
    assert_eq!(text, expected, "{text:#?}");
    // Styles, by element.
    let d = model::DUSK;
    assert_eq!(seg_of(&rows, "Cost").style.fg, Some(d.gold));
    assert!(has(seg_of(&rows, "Cost").style, Modifier::BOLD));
    let jobs = seg_of(&rows, "Known failed jobs").style;
    assert!(jobs.fg == Some(d.gold) && has(jobs, Modifier::ITALIC));
    let turkish = seg_of(&rows, "Turkish").style;
    assert!(turkish.fg == Some(d.ink) && has(turkish, Modifier::BOLD));
    let yesterday = seg_of(&rows, "yesterday's").style;
    assert!(yesterday.fg == Some(d.body) && has(yesterday, Modifier::ITALIC));
    let link = seg_of(&rows, "the /tr skill").style;
    assert!(link.fg == Some(d.accent) && has(link, Modifier::UNDERLINED));
    assert!(!text.iter().any(|row| row.contains("example.com")));
    assert_eq!(seg_of(&rows, "check.sh").style.fg, Some(d.gold));
    assert_eq!(seg_of(&rows, "1.").style.fg, Some(d.rust));
    assert_eq!(seg_of(&rows, "▎").style.fg, Some(d.rule));
    let quote = seg_of(&rows, "Nightly full passes must succeed within 48 h.").style;
    assert!(has(quote, Modifier::ITALIC));
    let code_row = &rows[23];
    assert!(code_row.segs[1..]
        .iter()
        .all(|seg| seg.style.bg == Some(d.sel)));
    assert_eq!(seg_of(&rows, "Bucket").style.fg, Some(d.ink));
    assert!(has(seg_of(&rows, "Bucket").style, Modifier::BOLD));
    assert_eq!(seg_of(&rows, " │ ").style.fg, Some(d.rule));
    assert_eq!(seg_of(&rows, "leap-bi4-backups").style.fg, Some(d.gold));
    assert_eq!(seg_of(&rows, &"─".repeat(56)).style.fg, Some(d.rule));
}

#[test]
fn every_markdown_row_fits_its_width_and_targets_a_char_boundary() {
    let lines: Vec<&str> = BRIEF_STUB.split('\n').collect();
    for width in [12, 20, 28, 41, 56, 80] {
        for row in md_rows(BRIEF_STUB, width) {
            let text = row.text();
            assert!(
                unicode_width::UnicodeWidthStr::width(text.as_str()) <= width,
                "{width}: {text:?}"
            );
            let targets = row
                .segs
                .iter()
                .filter_map(|seg| seg.target.as_ref())
                .chain(row.target.as_ref());
            for target in targets {
                let (line, byte) = match target {
                    InfoDockTarget::EditAt {
                        line, byte_start, ..
                    } => (*line, *byte_start),
                    InfoDockTarget::EditEnd { line, byte } => (*line, *byte),
                    _ => continue,
                };
                let source = lines[line].trim_end();
                assert!(byte <= source.len(), "{width}: {target:?}");
                assert!(source.is_char_boundary(byte), "{width}: {target:?}");
            }
        }
    }
}

#[test]
fn hidden_markers_keep_clicks_on_their_source_bytes() {
    let line = "Flags go to the owner in **Turkish**, see [the /tr skill](https://example.com/tr).";
    let rows = md_rows(line, 56);
    let link = seg_of(&rows, "the /tr skill");
    let at = line.find("the /tr").expect("link text");
    assert_eq!(
        link.target,
        Some(InfoDockTarget::EditAt {
            line: 0,
            byte_start: at,
            spaced: false,
        })
    );
    // Column 4 of the link text is its fifth char in the source.
    assert_eq!(model::edit_offset(line, at, 4, false), at + 4);
    let bold = seg_of(&rows, "Turkish");
    let at = line.find("Turkish").expect("bold text");
    assert_eq!(
        bold.target,
        Some(InfoDockTarget::EditAt {
            line: 0,
            byte_start: at,
            spaced: false,
        })
    );
    let after = seg_of(&rows, ".");
    assert_eq!(
        after.target,
        Some(InfoDockTarget::EditAt {
            line: 0,
            byte_start: line.len() - 1,
            spaced: false,
        })
    );
    // Past the text: the end of the source line, closing markers included.
    assert_eq!(
        rows[0].target,
        Some(InfoDockTarget::EditEnd {
            line: 0,
            byte: line.len(),
        })
    );
}

#[test]
fn unclosed_or_spaced_markers_stay_literal() {
    assert_eq!(
        md_text("2 * 3 * 4 and **open", 40)[0],
        "2 * 3 * 4 and **open"
    );
    assert_eq!(md_text("a `tick and [x](", 40)[0], "a `tick and [x](");
    assert_eq!(md_text("[](empty) **", 40)[0], "[](empty) **");
}

#[test]
fn headings_three_and_four_are_gold_and_wrap() {
    let text = md_text(
        "### A heading long enough to wrap\n#### And a smaller one too",
        16,
    );
    assert_eq!(
        text[..5],
        [
            "A heading long",
            "enough to wrap",
            "  And a smaller",
            "  one too",
            ""
        ]
    );
}

#[test]
fn numbered_items_align_their_wrapped_text_and_nest_bullets() {
    let text = md_text(
        "1. first item that wraps here\n10. tenth\n   - nested under\n   - [ ] a nested task\n2) paren",
        20,
    );
    assert_eq!(
        text[..6],
        [
            "1. first item that",
            "   wraps here",
            "10. tenth",
            "   · nested under",
            "   ○ a nested task",
            "2) paren",
        ]
    );
    // A nested item's wrap stays under its own text.
    let text = md_text("  - nested bullet text that wraps", 16);
    assert_eq!(
        text[..3],
        ["  · nested", "    bullet text", "    that wraps"]
    );
}

#[test]
fn quotes_and_rules_draw_their_bars() {
    let text = md_text("> a quote that wraps around\n---\n- --- not a rule", 16);
    assert_eq!(
        text[..5],
        [
            "  ▎ a quote that",
            "    wraps around",
            &"─".repeat(16),
            "· --- not a rule",
            ""
        ]
    );
}

#[test]
fn code_blocks_hide_their_fences_keep_lines_and_wrap() {
    let source = "```\nlet x = 1;\n    indented();\n```\nafter";
    let rows = md_rows(source, 20);
    let text: Vec<String> = rows.iter().map(model::Row::text).collect();
    assert_eq!(
        text[..3],
        [
            format!("  ▎ let x = 1;{}", " ".repeat(6)),
            "  ▎     indented(); ".to_owned(),
            "after".to_owned(),
        ]
    );
    // Clicks land on the code's own source line, never on a fence.
    assert_eq!(
        rows[1].target,
        Some(InfoDockTarget::EditEnd {
            line: 2,
            byte: "    indented();".len(),
        })
    );
    // An unclosed fence runs to the end; markdown inside is not parsed.
    let text = md_text("```\n# not a title\n- not a bullet", 30);
    assert!(text[0].starts_with("  ▎ # not a title"), "{text:?}");
    assert!(text[1].starts_with("  ▎ - not a bullet"), "{text:?}");
}

#[test]
fn tables_align_and_truncate_to_the_width() {
    let table =
        "| Name | Value |\n|---|:-:|\n| alpha | 1 |\n| a much longer name | 22 |\n| short |";
    let text = md_text(table, 40);
    assert_eq!(
        text[..5],
        [
            "Name               │ Value",
            "───────────────────┼──────",
            "alpha              │ 1",
            "a much longer name │ 22",
            "short              │ ",
        ]
    );
    let narrow = md_text(table, 14);
    assert_eq!(narrow[3], "a muc… │ 22");
    for row in &narrow[..5] {
        assert!(
            unicode_width::UnicodeWidthStr::width(row.as_str()) <= 14,
            "{row:?}"
        );
    }
    // Without a separator row there is no header.
    let rows = md_rows("| a | b |\n| c | d |", 20);
    assert_eq!(seg_of(&rows, "a").style.fg, Some(model::DUSK.body));
}

#[test]
fn the_plus_notes_stub_page_renders() {
    let stub = include_str!("golden/notes_stub.md");
    let text = md_text(stub, 56);
    let expected = [
        "❦ H E R D R +   N E X T   R O U N D".to_owned(),
        String::new(),
        "Planning the next batch of herdr+ features. The list is".to_owned(),
        "settled; notes cleanup goes first.".to_owned(),
        String::new(),
        rule_row("In flight", 56),
        String::new(),
        "○ Draft the /plus:notes skill".to_owned(),
        "● Pick the notes formatting (Sonnet directed)".to_owned(),
        String::new(),
        rule_row("Waiting on you", 56),
        String::new(),
        "· Agents keep their page current on their own, or only".to_owned(),
        "  on request?".to_owned(),
        String::new(),
        rule_row("Plan", 56),
        String::new(),
        "1. Notes: remove log, checkpoints and History".to_owned(),
        "2. Notes pane formatting".to_owned(),
        "3. Quiet messages, then do-not-disturb".to_owned(),
        "4. Sidebar: search, time filter, sink suspended".to_owned(),
        String::new(),
        rule_row("Reference", 56),
        String::new(),
        "· .local/prd/planned-features.md".to_owned(),
        "· .local/prd/notes-format/gallery.html".to_owned(),
        String::new(),
        rule_row("Notify", 56),
        String::new(),
        "Only when a build or audit finishes.".to_owned(),
        String::new(),
        String::new(),
        "❧ rev abc".to_owned(),
    ];
    assert_eq!(text, expected, "{text:#?}");
}

#[test]
fn a_very_wide_table_keeps_only_the_columns_that_fit() {
    let header: String = (0..5000).map(|i| format!("| c{i} ")).collect::<String>() + "|";
    let sep: String = "|---".repeat(5000) + "|";
    let row: String = (0..5000).map(|i| format!("| {i} ")).collect::<String>() + "|";
    let table = format!("{header}\n{sep}\n{row}");
    let started = std::time::Instant::now();
    let rows = md_rows(&table, 40);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    for row in &rows[..3] {
        let text = row.text();
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= 40,
            "{text:?}"
        );
        assert!(row.segs.len() < 64, "{}", row.segs.len());
    }
    assert!(rows[0].text().ends_with(" │ …"), "{:?}", rows[0].text());
    assert!(rows[1].text().ends_with("┼──"), "{:?}", rows[1].text());
    assert!(rows[2].text().starts_with("0 "), "{:?}", rows[2].text());
}

#[test]
fn a_huge_unbroken_word_wraps_in_linear_time() {
    let word = "x".repeat(256 * 1024);
    let started = std::time::Instant::now();
    let rows = md_rows(&word, 40);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // 256 KiB in rows of 40, plus the blank and meta rows.
    assert_eq!(rows.len(), 256 * 1024 / 40 + 1 + 2);
}

#[test]
fn only_a_bracket_followed_by_a_paren_is_a_link() {
    let rows = md_rows("[a] and [b](u)", 40);
    assert_eq!(rows[0].text(), "[a] and b");
    let link = seg_of(&rows, "b").style;
    assert!(has(link, ratatui::style::Modifier::UNDERLINED));
    assert_eq!(link.fg, Some(model::DUSK.accent));
    assert!(!rows[0]
        .segs
        .iter()
        .any(|seg| seg.text.contains('a') && has(seg.style, ratatui::style::Modifier::UNDERLINED)));
}

#[test]
fn rows_rebuild_on_a_utc_offset_change_not_on_the_clock() {
    let mut state = dock_state();
    open_loaded(&mut state);
    let area = state.info_dock_area();
    let input = super::super::info_dock::DockComposeInput {
        tab_label: "calendar-fix",
        sized_elsewhere: false,
    };
    let dock = state.info_dock.as_deref_mut().expect("dock");
    dock.compute_view(area, &input, 3600);
    let built = dock.rows_built;
    // The same inputs a minute (or an hour) later: the memo holds.
    dock.compute_view(area, &input, 3600);
    dock.compute_view(area, &input, 3600);
    assert_eq!(dock.rows_built, built);
    // A DST change moves the offset: the `updated` time is redrawn.
    dock.compute_view(area, &input, 7200);
    assert_eq!(dock.rows_built, built + 1);
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
        assert!(rows.iter().any(|row| row.contains("✦ Notes ✦")), "{rows:?}");
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
    state.info_dock.as_deref_mut().expect("dock").focused = true;
    let outcome = state.handle_raw_events(vec![key(KeyCode::Char('j'))]);
    assert_eq!(pane_requests(&outcome), 0, "j scrolls the notes");
    let scroll = state
        .info_dock
        .as_deref_mut()
        .and_then(|dock| dock.ui.get(KEY).map(|ui| ui.notes_scroll));
    assert_eq!(scroll, Some(1));
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
fn the_editor_saves_on_esc() {
    let mut state = dock_state();
    start_dirty_edit(&mut state);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Esc)]);
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
    let [(id, Method::NotesGet(_))] = &requests[..] else {
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
        [(_, Method::NotesGet(_))]
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
    dock.queue(
        super::super::info_dock::InfoRequest::NotesSet {
            key: KEY.into(),
            text: "x".into(),
            base_revision: None,
            tick: false,
        },
        None,
    );
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
            Ok(ResponseResult::NotesGet {
                notes: notes_info(NOTES, "sha256:4e9b1c07a2f3"),
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
        // Fork (menu design): Info pane leads with Close, before Important.
        assert!(
            index < close && close < important,
            "before Close and Important"
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

    // The focused tab: the dock opens and the surface narrows; the session
    // keeps the keyboard.
    let mut state = dock_state();
    let outcome = pick_info_pane(&mut state, "tab_1");
    assert!(state.info_dock_open_for_focused_tab());
    assert!(outcome.resize);
    recompose(&mut state);
    assert!(!state.info_dock_focused());

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

/// The server sizes only the tab shown, so the client reports its surface
/// size when it shows another tab, even at an unchanged size; a snapshot that
/// keeps the tab and the size reports nothing.
#[test]
fn showing_another_tab_reports_the_surface_size_once() {
    let mut state = dock_state();
    let before = state.surface_report_basis(COLS, ROWS);
    state.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
    assert!(!state.surface_report_required(&before, COLS, ROWS));

    let before = state.surface_report_basis(COLS, ROWS);
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    assert!(state.surface_report_required(&before, COLS, ROWS));
    assert_eq!(
        state.surface_report_basis(COLS, ROWS).size,
        before.size,
        "same surface size, another tab"
    );

    let before = state.surface_report_basis(COLS, ROWS);
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    assert!(!state.surface_report_required(&before, COLS, ROWS));
}

#[test]
fn the_first_snapshot_reports_only_a_size_change() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let before = state.surface_report_basis(COLS, ROWS);
    state.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
    assert_eq!(
        state.surface_report_required(&before, COLS, ROWS),
        before.size != state.surface_size(COLS, ROWS)
    );
}

#[test]
fn opening_the_info_pane_never_moves_the_keyboard_focus() {
    let mut config = Config::default();
    config.keys.toggle_info_pane = crate::config::BindingConfig::one("alt+n");
    let mut state = state_with(config);
    let alt_n = || {
        vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('n'),
            KeyModifiers::ALT,
        ))]
    };
    // alt+n opens the focused tab's dock; keys still go to the session.
    let opened = state.handle_raw_events(alt_n());
    answer_in_flight(&mut state, &opened, |_| ResponseResult::NotesGet {
        notes: notes_info(NOTES, "sha256:4e9b1c07a2f3"),
    });
    recompose(&mut state);
    assert!(state.info_dock_open_for_focused_tab());
    assert!(!state.info_dock_focused());
    let typed = state.handle_raw_events(vec![key(KeyCode::Char('j'))]);
    assert_eq!(pane_requests(&typed), 1, "j goes to the pane");

    // A click into the dock focuses it.
    let body = state.hits.info_dock.body;
    state.handle_raw_events(click(body.x + 5, body.y + 3));
    assert!(state.info_dock_focused());

    // alt+n again closes it and drops its focus; keys go to the pane again.
    state.handle_raw_events(alt_n());
    recompose(&mut state);
    assert!(!state.info_dock_open_for_focused_tab());
    assert!(!state.info_dock.as_deref().expect("dock").focused);
    let typed = state.handle_raw_events(vec![key(KeyCode::Char('k'))]);
    assert_eq!(pane_requests(&typed), 1);

    // Reopened: still not focused (the earlier click does not carry over).
    let reopened = state.handle_raw_events(alt_n());
    recompose(&mut state);
    let _ = reopened;
    assert!(state.info_dock_open_for_focused_tab());
    assert!(!state.info_dock_focused());
}

#[test]
fn a_notes_reveal_opens_the_tabs_pane_once_without_taking_the_keyboard() {
    use crate::server::headless::notes_reveal::NotesRevealPayload;
    let payload = |boot_id: &str, revision, tab_id: &str| NotesRevealPayload {
        boot_id: boot_id.into(),
        revision,
        tab_id: tab_id.into(),
    };
    let local = ClientEndpointId::Local;
    let mut state = dock_state();
    // The focused tab: the dock opens (the surface narrows), unfocused.
    let outcome = state.receive_notes_reveal(&local, payload("boot-1", 1, "tab_1"));
    assert!(outcome.repaint && outcome.resize);
    assert!(state.info_dock_open_for_focused_tab());
    recompose(&mut state);
    assert!(!state.info_dock_focused());
    let typed = state.handle_raw_events(vec![key(KeyCode::Char('j'))]);
    assert_eq!(pane_requests(&typed), 1, "keys stay with the session");

    // Already open: nothing changes (a reveal never closes it).
    let again = state.receive_notes_reveal(&local, payload("boot-1", 2, "tab_1"));
    assert!(!again.repaint && !again.resize);
    assert!(state.info_dock_open_for_focused_tab());

    // Closed by the user, an old or repeated revision does not reopen it; a
    // newer one does.
    toggle(&mut state);
    assert!(!state.info_dock_open_for_focused_tab());
    state.receive_notes_reveal(&local, payload("boot-1", 2, "tab_1"));
    assert!(!state.info_dock_open_for_focused_tab(), "seen already");
    state.receive_notes_reveal(&local, payload("boot-1", 3, "tab_1"));
    assert!(state.info_dock_open_for_focused_tab());

    // A background tab opens in state only.
    let background = state.receive_notes_reveal(&local, payload("boot-1", 4, "tab_2"));
    assert!(!background.resize);
    assert!(state.info_dock.as_deref().is_some_and(|dock| dock
        .open_tabs
        .contains(&(local.clone(), "tab_2".to_owned()))));

    // A new server boot starts its revisions over.
    toggle(&mut state);
    state.receive_notes_reveal(&local, payload("boot-2", 1, "tab_1"));
    assert!(state.info_dock_open_for_focused_tab());
    recompose(&mut state);
    assert!(!state.info_dock_focused());
}

/// Tab A's dock has the keyboard (a click), then tab B is focused with the
/// `next_tab` key (the server's answer is the new snapshot).
fn focused_dock_then_tab_b() -> ClientShellState {
    let mut config = Config::default();
    config.keys.toggle_info_pane = crate::config::BindingConfig::one("alt+n");
    config.keys.next_tab = crate::config::BindingConfig::one("alt+l");
    let mut state = state_with(config);
    open_loaded(&mut state);
    let body = state.hits.info_dock.body;
    state.handle_raw_events(click(body.x + 5, body.y + 3));
    assert!(state.info_dock_focused(), "tab A's dock has the keyboard");
    let switched = state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('l'),
        KeyModifiers::ALT,
    ))]);
    assert!(
        !info_requests(&switched).is_empty(),
        "the key asks the server to focus the next tab"
    );
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    recompose(&mut state);
    state
}

#[test]
fn opening_another_tabs_pane_drops_a_focus_left_from_the_first() {
    let mut state = focused_dock_then_tab_b();
    state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('n'),
        KeyModifiers::ALT,
    ))]);
    recompose(&mut state);
    assert!(state.info_dock_open_for_focused_tab());
    assert!(!state.info_dock_focused());
    let typed = state.handle_raw_events(vec![key(KeyCode::Char('j'))]);
    assert_eq!(pane_requests(&typed), 1, "keys go to tab B's pane");
}

#[test]
fn a_reveal_of_another_tab_drops_a_focus_left_from_the_first() {
    use crate::server::headless::notes_reveal::NotesRevealPayload;
    let mut state = focused_dock_then_tab_b();
    state.receive_notes_reveal(
        &ClientEndpointId::Local,
        NotesRevealPayload {
            boot_id: "boot-1".into(),
            revision: 1,
            tab_id: "tab_2".into(),
        },
    );
    recompose(&mut state);
    assert!(state.info_dock_open_for_focused_tab());
    assert!(!state.info_dock_focused());
}
