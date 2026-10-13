//! Engine tests for the OSC 7501 parser and record store. The fixtures are
//! report sequences Claude Code 2.1.296 emitted in the spike's live runs
//! (decoded in the spike, re-encoded here); they exercise the engine, not
//! Claude's detection rules.

use std::time::{Duration, Instant};

use super::*;
use crate::pane::osc::AgentOscStateTracker;

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

fn set(payload: &str) -> (String, ProgramStatusRecord) {
    match parse_program_status(payload.as_bytes()) {
        Ok(ProgramStatusReport::Set { id, record }) => (id, record),
        other => panic!("expected a set report for {payload:?}, got {other:?}"),
    }
}

fn reject(payload: &[u8]) -> ProgramStatusReject {
    match parse_program_status(payload) {
        Err(reject) => reject,
        Ok(report) => panic!("expected a reject, got {report:?}"),
    }
}

#[test]
fn parses_the_spikes_raw_permission_report() {
    let (id, record) = set(
        "state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZSBCYXNoOiB0b3VjaCAvdG1wL29zYy1zcGlrZS9ydW4veC50eHQ=",
    );
    assert_eq!(id, "");
    assert_eq!(record.state, ProgramState::Blocked);
    assert_eq!(record.kind, Some(ProgramStatusKind::Permission));
    assert_eq!(record.app.as_deref(), Some("claude-code"));
    assert_eq!(
        record.msg.as_deref(),
        Some("approve Bash: touch /tmp/osc-spike/run/x.txt")
    );
}

#[test]
fn accepts_blocked_without_kind() {
    let (_, record) = set("state=blocked:app=claude-code");
    assert_eq!(record.state, ProgramState::Blocked);
    assert_eq!(record.kind, None);
}

#[test]
fn is_lenient_where_the_spec_allows() {
    // Unknown key, malformed pair, a value outside the charset, repeated key.
    let (_, record) = set("future=1:garbage:title=a b:state=idle:state=working:app=x");
    assert_eq!(record.state, ProgramState::Working);
    assert_eq!(record.title, None);
    assert_eq!(record.app.as_deref(), Some("x"));
    // A bad key charset is skipped, not rejected.
    let (_, record) = set("bad.key=1:state=done");
    assert_eq!(record.state, ProgramState::Done);
}

#[test]
fn keeps_kind_and_progress_only_where_they_apply() {
    assert_eq!(set("state=working:progress=150").1.progress, Some(100));
    assert_eq!(set("state=blocked:progress=7").1.progress, Some(7));
    assert_eq!(set("state=done:progress=50").1.progress, None);
    assert_eq!(set("state=working:progress=x1").1.progress, None);
    assert_eq!(set("state=working:progress=1000").1.progress, None);
    assert_eq!(set("state=working:kind=permission").1.kind, None);
    assert_eq!(
        set("state=blocked:kind=sandbox").1.kind,
        Some(ProgramStatusKind::Other)
    );
    assert_eq!(
        set("state=blocked:kind=question").1.kind,
        Some(ProgramStatusKind::Question)
    );
    assert_eq!(
        set("state=blocked:kind=auth").1.kind,
        Some(ProgramStatusKind::Auth)
    );
}

#[test]
fn decodes_unpadded_and_url_safe_base64() {
    let unpadded = b64("hi there").trim_end_matches('=').to_string();
    assert_eq!(
        set(&format!("state=working:msg={unpadded}"))
            .1
            .msg
            .as_deref(),
        Some("hi there")
    );
    // "??>" encodes to "Pz8+" (standard) and "Pz8-" (URL-safe).
    assert_eq!(set("state=working:msg=Pz8-").1.msg.as_deref(), Some("??>"));
}

#[test]
fn strips_bidi_and_invisible_characters() {
    let msg = b64("a\u{202E}b\u{200B}c\u{2066}d\u{FEFF}e\u{2028}f");
    assert_eq!(
        set(&format!("state=working:msg={msg}")).1.msg.as_deref(),
        Some("abcdef")
    );
    let only_invisible = b64("\u{200B}\u{200F}");
    assert_eq!(
        set(&format!("state=working:title={only_invisible}"))
            .1
            .title,
        None
    );
}

#[test]
fn parses_ids_and_clears() {
    let (id, _) = set("state=working:id=task/sub-1");
    assert_eq!(id, "task/sub-1");
    assert_eq!(
        parse_program_status(b"state=clear"),
        Ok(ProgramStatusReport::Clear { id: None })
    );
    assert_eq!(
        parse_program_status(b"state=clear:id=a5a666839654a9adb:msg=!!!"),
        Ok(ProgramStatusReport::Clear {
            id: Some("a5a666839654a9adb".into())
        })
    );
}

#[test]
fn rejects_whole_reports_on_violations() {
    let segment33 = "a".repeat(33);
    let id129 = vec!["a".repeat(32); 4].join("/") + "/x";
    let cases: Vec<(Vec<u8>, ProgramStatusReject)> = vec![
        (b"app=x".to_vec(), ProgramStatusReject::MissingState),
        (b"state=paused".to_vec(), ProgramStatusReject::UnknownState),
        (
            b"state=idle:abcdefghijklmnopq=1".to_vec(),
            ProgramStatusReject::KeyTooLong,
        ),
        (
            b"state=idle:id=1/2/3/4/5/6/7/8/9".to_vec(),
            ProgramStatusReject::IdLimit,
        ),
        (
            format!("state=idle:id={segment33}").into_bytes(),
            ProgramStatusReject::IdLimit,
        ),
        (
            format!("state=idle:id={id129}").into_bytes(),
            ProgramStatusReject::IdLimit,
        ),
        (b"state=idle:id=a//b".to_vec(), ProgramStatusReject::IdLimit),
        (
            format!("state=idle:app={}", "a".repeat(33)).into_bytes(),
            ProgramStatusReject::AppTooLong,
        ),
        (
            format!("state=idle:msg={}", "A".repeat(2733)).into_bytes(),
            ProgramStatusReject::MsgTooLong,
        ),
        (
            format!("state=idle:msg={}", b64(&"x".repeat(2049))).into_bytes(),
            ProgramStatusReject::MsgTooLong,
        ),
        (
            format!("state=idle:title={}", b64(&"x".repeat(193))).into_bytes(),
            ProgramStatusReject::TitleTooLong,
        ),
        (b"state=idle:msg=A".to_vec(), ProgramStatusReject::Base64),
        (b"state=idle:msg=/w==".to_vec(), ProgramStatusReject::Utf8),
        (
            format!("state=idle:msg={}", b64("a\x1bb")).into_bytes(),
            ProgramStatusReject::ControlChar,
        ),
        (
            format!("state=idle:msg={}", b64("a\nb")).into_bytes(),
            ProgramStatusReject::ControlChar,
        ),
        (
            format!("state=idle:msg={}", b64("a\u{0085}b")).into_bytes(),
            ProgramStatusReject::ControlChar,
        ),
        // An interrupted sequence carries raw ESC and later bytes.
        (
            b"state=idle\x1b[c:state=working".to_vec(),
            ProgramStatusReject::ControlChar,
        ),
        (b"state=idle\x7f".to_vec(), ProgramStatusReject::ControlChar),
        (
            format!("state=idle:pad={}", "a".repeat(4080)).into_bytes(),
            ProgramStatusReject::TooLong,
        ),
    ];
    for (payload, expected) in cases {
        assert_eq!(
            reject(&payload),
            expected,
            "{:?}",
            String::from_utf8_lossy(&payload[..payload.len().min(60)])
        );
    }
}

#[test]
fn the_probe_is_exactly_a_question_mark() {
    assert!(is_probe(b"?"));
    assert!(!is_probe(b"?:states=idle"));
    assert!(!is_probe(b"?\x1b[c"));
    assert!(!is_probe(b""));
}

fn report(payload: &str) -> ProgramStatusReport {
    parse_program_status(payload.as_bytes()).expect("valid report")
}

#[test]
fn store_replaces_clears_subtrees_and_everything() {
    let now = Instant::now();
    let mut store = ProgramStatusStore::default();
    store.apply(report("state=working:app=claude-code:msg=YQ=="), now);
    store.apply(report("state=done"), now);
    let root = store.root().expect("root");
    assert_eq!(root.state, ProgramState::Done);
    assert_eq!(root.msg, None, "a report replaces its record entirely");
    assert_eq!(root.app, None);

    store.apply(report("state=working:id=p"), now);
    store.apply(report("state=working:id=p/q"), now);
    store.apply(report("state=working:id=pX"), now);
    store.apply(report("state=clear:id=p"), now);
    let snapshot = store.snapshot(now);
    let ids: Vec<_> = snapshot.children.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["pX"]);
    assert!(snapshot.root.is_some());

    store.apply(report("state=clear"), now);
    assert!(store.snapshot(now).root.is_none());
    assert!(store.snapshot(now).children.is_empty());
}

#[test]
fn store_evicts_the_least_recent_child_never_the_root() {
    let now = Instant::now();
    let mut store = ProgramStatusStore::default();
    store.apply(report("state=idle"), now);
    for index in 0..MAX_RECORDS {
        store.apply(report(&format!("state=working:id=c{index}")), now);
    }
    let snapshot = store.snapshot(now);
    assert!(snapshot.root.is_some());
    assert_eq!(snapshot.children.len(), MAX_RECORDS - 1);
    assert!(!snapshot.children.iter().any(|child| child.id == "c0"));
    assert!(snapshot.children.iter().any(|child| child.id == "c1"));
}

#[test]
fn store_revision_moves_only_on_change() {
    let now = Instant::now();
    let mut store = ProgramStatusStore::default();
    store.apply(report("state=clear"), now);
    store.apply(report("state=clear:id=x"), now);
    store.clear_records();
    assert_eq!(store.revision(), 0);
    store.apply(report("state=idle"), now);
    assert_eq!(store.revision(), 1);
    store.note_reject(ProgramStatusReject::Base64, now);
    assert_eq!(store.revision(), 2, "a reject is visible to the logger");
}

#[test]
fn store_resolves_app_from_the_nearest_ancestor() {
    let now = Instant::now();
    let mut store = ProgramStatusStore::default();
    store.apply(report("state=idle:app=root-app"), now);
    store.apply(report("state=working:id=a:app=a-app"), now);
    store.apply(report("state=working:id=a/b"), now);
    store.apply(report("state=working:id=z"), now);
    let snapshot = store.snapshot(now);
    let app = |id: &str| {
        snapshot
            .children
            .iter()
            .find(|child| child.id == id)
            .and_then(|child| child.app.clone())
    };
    assert_eq!(app("a/b").as_deref(), Some("a-app"));
    assert_eq!(app("z").as_deref(), Some("root-app"));
}

#[test]
fn a_probe_clears_records_and_marks_the_probe_answered() {
    let now = Instant::now();
    let mut store = ProgramStatusStore::default();
    store.apply(report("state=blocked"), now);
    store.note_probe();
    let snapshot = store.snapshot(now);
    assert!(snapshot.root.is_none());
    assert!(snapshot.probe_answered);
    store.clear_retained();
    assert!(!store.snapshot(now).probe_answered);
}

#[test]
fn packed_root_round_trips() {
    for state in [
        None,
        Some(ProgramState::Idle),
        Some(ProgramState::Working),
        Some(ProgramState::Done),
        Some(ProgramState::Blocked),
        Some(ProgramState::Error),
    ] {
        let root = PackedRoot {
            state,
            children: 33,
            rejected: 200,
            revision: 123_456,
        };
        assert_eq!(unpack_root(pack_root(root)), root);
    }
    let mut store = ProgramStatusStore::default();
    assert_eq!(store.take_packed_if_changed(), None);
    store.apply(report("state=blocked"), Instant::now());
    let packed = store.take_packed_if_changed().expect("changed");
    assert_eq!(unpack_root(packed).state, Some(ProgramState::Blocked));
    assert_eq!(store.take_packed_if_changed(), None);
}

#[test]
fn explain_json_compares_with_the_screen() {
    let now = Instant::now();
    let mut store = ProgramStatusStore::default();
    store.note_probe();
    store.apply(
        report(&format!(
            "state=blocked:app=claude-code:kind=permission:msg={}",
            b64("approve Bash: touch /tmp/x.txt")
        )),
        now,
    );
    store.apply(
        report(&format!(
            "state=working:id=a5a666839654a9adb:title={}",
            b64("say ok")
        )),
        now,
    );
    let snapshot = store.snapshot(now + Duration::from_millis(5));
    let json = snapshot.to_json(Some(AgentState::Blocked));
    assert_eq!(json["probe_answered"], true);
    assert_eq!(json["root"]["state"], "blocked");
    assert_eq!(json["root"]["kind"], "permission");
    assert_eq!(json["root"]["msg"], "approve Bash: touch /tmp/x.txt");
    assert_eq!(json["mapped_state"], "blocked");
    assert_eq!(json["agrees_with_screen"], true);
    assert_eq!(json["children"][0]["id"], "a5a666839654a9adb");
    assert_eq!(json["children"][0]["app"], "claude-code");
    assert_eq!(json["children"][0]["title"], "say ok");
    assert_eq!(
        snapshot.to_json(Some(AgentState::Idle))["agrees_with_screen"],
        false
    );
    assert!(snapshot.to_json(Some(AgentState::Unknown))["agrees_with_screen"].is_null());
    assert!(snapshot.to_json(None)["agrees_with_screen"].is_null());
}

// --- tracker (collector integration) ---

fn osc(payload: &str) -> Vec<u8> {
    format!("\x1b]7501;{payload}\x07").into_bytes()
}

fn root_state(tracker: &AgentOscStateTracker) -> Option<ProgramState> {
    tracker.program_status().root().map(|record| record.state)
}

#[test]
fn tracker_applies_a_report_split_across_chunks_once() {
    let mut tracker = AgentOscStateTracker::default();
    let bytes = osc("state=working:app=claude-code");
    let (a, rest) = bytes.split_at(4);
    let (b, c) = rest.split_at(10);
    tracker.observe(a);
    tracker.observe(b);
    assert_eq!(root_state(&tracker), None);
    tracker.observe(c);
    assert_eq!(root_state(&tracker), Some(ProgramState::Working));
    assert_eq!(tracker.program_status().revision(), 1);
}

#[test]
fn tracker_clears_on_ris_but_not_on_soft_reset() {
    let mut tracker = AgentOscStateTracker::default();
    tracker.observe(&osc("state=working"));
    tracker.observe(b"\x1b[!p");
    assert_eq!(root_state(&tracker), Some(ProgramState::Working));
    tracker.observe(b"\x1bc");
    assert_eq!(root_state(&tracker), None);
}

#[test]
fn tracker_clear_retained_resets_records_and_probe() {
    let mut tracker = AgentOscStateTracker::default();
    tracker.observe(&osc("?"));
    tracker.observe(&osc("state=idle"));
    assert_eq!(tracker.take_program_status_probe(), Some(9));
    tracker.clear_retained();
    let snapshot = tracker.program_status().snapshot(Instant::now());
    assert!(snapshot.root.is_none());
    assert!(!snapshot.probe_answered);
}

#[test]
fn tracker_ignores_an_oversize_body() {
    let mut tracker = AgentOscStateTracker::default();
    tracker.observe(&osc(&format!("state=working:pad={}", "a".repeat(5000))));
    assert_eq!(root_state(&tracker), None);
    assert_eq!(tracker.program_status().revision(), 0);
}

#[test]
fn tracker_answers_one_probe_per_chunk() {
    let mut tracker = AgentOscStateTracker::default();
    let mut chunk = Vec::new();
    for _ in 0..50 {
        chunk.extend(osc("?"));
    }
    tracker.observe(&chunk);
    assert_eq!(tracker.take_program_status_probe(), Some(9));
    assert_eq!(tracker.take_program_status_probe(), None);
}

#[test]
fn tracker_ignores_an_interrupted_probe() {
    let mut tracker = AgentOscStateTracker::default();
    tracker.observe(b"\x1b]7501;?\x1b[c");
    tracker.observe(b"later\x07");
    assert_eq!(tracker.take_program_status_probe(), None);
    assert!(
        !tracker
            .program_status()
            .snapshot(Instant::now())
            .probe_answered
    );
}

/// Spike run1 (Claude Code 2.1.296): permission, then a question, then exit.
fn run1() -> Vec<u8> {
    let mut bytes = osc("?");
    for payload in [
        "state=idle:app=claude-code".to_string(),
        "state=working:app=claude-code".to_string(),
        format!(
            "state=working:app=claude-code:msg={}",
            b64("Creating empty file x.txt in /tmp/osc-spike/run")
        ),
        "state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZSBCYXNoOiB0b3VjaCAvdG1wL29zYy1zcGlrZS9ydW4veC50eHQ=".to_string(),
        "state=working:app=claude-code".to_string(),
        "state=done:app=claude-code".to_string(),
        "state=working:app=claude-code".to_string(),
        format!(
            "state=blocked:app=claude-code:kind=question:msg={}",
            b64("answer: Do you prefer tea or coffee? (Tea · Coffee)")
        ),
        "state=idle:app=claude-code".to_string(),
        "state=clear".to_string(),
        "state=clear".to_string(),
        "state=clear".to_string(),
    ] {
        bytes.extend(b"some screen output\r\n");
        bytes.extend(osc(&payload));
    }
    bytes
}

fn replay_mapped(bytes: &[u8], byte_by_byte: bool) -> Vec<Option<AgentState>> {
    let mut tracker = AgentOscStateTracker::default();
    let mut seen = Vec::new();
    let mut last_revision = 0;
    let mut observe = |chunk: &[u8], tracker: &mut AgentOscStateTracker| {
        tracker.observe(chunk);
        let revision = tracker.program_status().revision();
        if revision != last_revision {
            last_revision = revision;
            seen.push(
                tracker
                    .program_status()
                    .root()
                    .map(|record| mapped_agent_state(record.state)),
            );
        }
    };
    if byte_by_byte {
        for byte in bytes {
            observe(std::slice::from_ref(byte), &mut tracker);
        }
    } else {
        observe(bytes, &mut tracker);
    }
    seen
}

#[test]
fn run1_fixture_maps_like_the_spike_observed() {
    use AgentState::{Blocked, Idle, Working};
    let expected = vec![
        None, // the probe
        Some(Idle),
        Some(Working),
        Some(Working),
        Some(Blocked),
        Some(Working),
        Some(Idle),
        Some(Working),
        Some(Blocked),
        Some(Idle),
        None, // the first clear; the next two change nothing
    ];
    assert_eq!(replay_mapped(&run1(), true), expected);
    // In one chunk only the end state is observable.
    assert_eq!(replay_mapped(&run1(), false), vec![None]);
}

#[test]
fn run2_fixture_tracks_the_background_subagent_child() {
    let mut tracker = AgentOscStateTracker::default();
    tracker.observe(&osc("?"));
    tracker.observe(&osc("state=idle:app=claude-code"));
    tracker.observe(&osc("state=working:app=claude-code"));
    tracker.observe(&osc(&format!(
        "state=working:app=claude-code:id=a5a666839654a9adb:title={}",
        b64("say ok")
    )));
    let snapshot = tracker.program_status().snapshot(Instant::now());
    assert_eq!(snapshot.children.len(), 1);
    assert_eq!(snapshot.children[0].id, "a5a666839654a9adb");
    assert_eq!(snapshot.children[0].record.title.as_deref(), Some("say ok"));
    tracker.observe(&osc("state=clear:id=a5a666839654a9adb"));
    tracker.observe(&osc("state=done:app=claude-code"));
    let snapshot = tracker.program_status().snapshot(Instant::now());
    assert!(snapshot.children.is_empty());
    assert_eq!(
        snapshot.root.map(|root| root.record.state),
        Some(ProgramState::Done)
    );
    tracker.observe(&osc("state=clear"));
    assert_eq!(root_state(&tracker), None);
}

#[test]
fn switch_off_ignores_reports_and_hides_records() {
    let mut tracker = AgentOscStateTracker::default();
    tracker.observe(&osc("state=blocked"));
    let off = SwitchOffForTest::new();
    tracker.observe(&osc("?"));
    tracker.observe(&osc("state=working"));
    let probe = tracker.take_program_status_probe();
    let hidden = tracker.program_status().snapshot(Instant::now());
    drop(off);
    assert_eq!(probe, None, "no reply while off");
    assert!(hidden.root.is_none(), "records hidden while off");
    assert!(!hidden.enabled);
    // Back on: the record from before is dropped, not shown again.
    assert!(tracker
        .program_status()
        .snapshot(Instant::now())
        .root
        .is_none());
    // Every read side agrees before the next report syncs the epoch: the
    // stage 2 region text included.
    assert!(tracker.program_status().root().is_none());
    assert_eq!(tracker.program_status().root_region_text(), "");
    tracker.observe(&osc("state=idle"));
    assert_eq!(root_state(&tracker), Some(ProgramState::Idle));
}
