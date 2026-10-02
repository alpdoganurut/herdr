//! Store tests: temp directories only, no PTYs.

use std::path::PathBuf;

use super::checkpoints::{
    AnchorRecord, CheckpointPatch, CheckpointStore, NewCheckpoint, MAX_ADDS_PER_HOUR,
    MAX_PARSE_BYTES,
};
use super::*;
use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind};
use crate::api::schema::notes::{CheckpointKind, NotesAuthor, NotesWriteOutcome};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-notes-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        Self(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn by(author: NotesAuthor) -> WriteBy<'static> {
    WriteBy {
        author,
        previous_key: None,
        now: 1_800_000_000,
    }
}

fn new_checkpoint(kind: CheckpointKind, title: &str) -> NewCheckpoint {
    NewCheckpoint {
        kind,
        author: NotesAuthor::Agent,
        title: title.into(),
        detail: None,
        tags: Vec::new(),
        anchor: None,
    }
}

#[test]
fn set_is_compare_and_swap() {
    let dir = TempDir::new("cas");
    let mut store = NotesStore::new(dir.path());

    let missing = store.get("tab-w1-t1", None).unwrap();
    assert!(!missing.exists);
    assert_eq!(missing.revision, NO_REVISION);
    assert_eq!(missing.text.as_deref(), Some(""));

    let (outcome, written) = store
        .set("tab-w1-t1", "one\n", None, by(NotesAuthor::User))
        .unwrap();
    assert_eq!(outcome, NotesWriteOutcome::Written);
    assert_eq!(written.revision, revision_of("one\n"));
    assert!(written.revision.starts_with("sha256:"));
    assert_eq!(written.revision.len(), "sha256:".len() + 12);
    assert_eq!(written.meta.updated_by, Some(NotesAuthor::User));

    // `None` only matches missing notes.
    let (outcome, current) = store
        .set("tab-w1-t1", "two\n", None, by(NotesAuthor::Agent))
        .unwrap();
    assert_eq!(outcome, NotesWriteOutcome::Conflict);
    assert_eq!(current.text.as_deref(), Some("one\n"));
    assert_eq!(current.revision, written.revision);

    let (outcome, _) = store
        .set(
            "tab-w1-t1",
            "two\n",
            Some("sha256:000000000000"),
            by(NotesAuthor::Agent),
        )
        .unwrap();
    assert_eq!(outcome, NotesWriteOutcome::Conflict);

    let (outcome, second) = store
        .set(
            "tab-w1-t1",
            "two\n",
            Some(&written.revision),
            by(NotesAuthor::Agent),
        )
        .unwrap();
    assert_eq!(outcome, NotesWriteOutcome::Written);

    let (outcome, same) = store
        .set(
            "tab-w1-t1",
            "two\n",
            Some(&second.revision),
            by(NotesAuthor::Agent),
        )
        .unwrap();
    assert_eq!(outcome, NotesWriteOutcome::Unchanged);
    assert_eq!(same.revision, second.revision);

    let known = store.get("tab-w1-t1", Some(&second.revision)).unwrap();
    assert!(known.text.is_none(), "a known revision omits the text");
    let fresh = store.get("tab-w1-t1", Some("sha256:stale")).unwrap();
    assert_eq!(fresh.text.as_deref(), Some("two\n"));
}

#[test]
fn an_external_edit_between_get_and_set_conflicts() {
    let dir = TempDir::new("external");
    let mut store = NotesStore::new(dir.path());
    let (_, written) = store
        .set("tab-w1-t1", "mine\n", None, by(NotesAuthor::User))
        .unwrap();
    let seen = store.get("tab-w1-t1", None).unwrap();
    assert_eq!(seen.revision, written.revision);

    fs::write(store.notes_path("tab-w1-t1"), "edited in vim\n").unwrap();
    let (outcome, current) = store
        .set(
            "tab-w1-t1",
            "mine, changed\n",
            Some(&seen.revision),
            by(NotesAuthor::User),
        )
        .unwrap();
    assert_eq!(outcome, NotesWriteOutcome::Conflict);
    assert_eq!(current.text.as_deref(), Some("edited in vim\n"));
}

#[test]
fn append_adds_to_a_section_and_stamps() {
    let dir = TempDir::new("append");
    let mut store = NotesStore::new(dir.path());
    let key = "claude-abc";
    store
        .set(
            key,
            "# title\n\n## Plan\n- a\n\n## Log\n- x\n",
            None,
            by(NotesAuthor::User),
        )
        .unwrap();

    let (outcome, appended) = store
        .append(key, "b", Some("Plan"), None, by(NotesAuthor::Agent))
        .unwrap();
    assert_eq!(outcome, NotesWriteOutcome::Written);
    assert_eq!(
        appended.text.as_deref(),
        Some("# title\n\n## Plan\n- a\nb\n\n## Log\n- x\n")
    );
    assert_eq!(
        appended.revision,
        revision_of(appended.text.as_deref().unwrap())
    );

    let (_, created) = store
        .append(
            key,
            "found it",
            Some("Findings"),
            Some("12:03"),
            by(NotesAuthor::Agent),
        )
        .unwrap();
    assert_eq!(
        created.text.as_deref(),
        Some("# title\n\n## Plan\n- a\nb\n\n## Log\n- x\n\n## Findings\n- 12:03 found it\n")
    );

    let (_, at_end) = store
        .append(key, "tail", None, None, by(NotesAuthor::Agent))
        .unwrap();
    assert!(at_end
        .text
        .as_deref()
        .unwrap()
        .ends_with("found it\ntail\n"));
    assert_eq!(store.get(key, None).unwrap().revision, at_end.revision);

    assert!(matches!(
        store.append(key, "  \n", None, None, by(NotesAuthor::Agent)),
        Err(NotesError::Invalid(_))
    ));
}

#[test]
fn append_text_places_lines_before_the_next_section() {
    assert_eq!(append_text("", "x", Some("Plan")), "## Plan\nx\n");
    assert_eq!(append_text("a", "x", None), "a\nx\n");
    assert_eq!(
        append_text("## Plan\n\n\n## Next\n", "x", Some("Plan")),
        "## Plan\nx\n\n\n## Next\n"
    );
    assert_eq!(
        append_text("## Plan\n- a", "x", Some("Plan")),
        "## Plan\n- a\nx\n"
    );
    // `### Plan` is not the section; the heading is created.
    assert_eq!(
        append_text("### Plan\n", "x", Some("Plan")),
        "### Plan\n\n## Plan\nx\n"
    );
}

#[test]
fn notes_over_the_cap_are_refused() {
    let dir = TempDir::new("cap");
    let mut store = NotesStore::new(dir.path());
    let big = "x".repeat(MAX_NOTES_BYTES as usize + 1);
    assert!(matches!(
        store.set("tab-w1-t1", &big, None, by(NotesAuthor::User)),
        Err(NotesError::TooLarge(_))
    ));
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(store.notes_path("tab-w1-t1"), &big).unwrap();
    let err = store.get("tab-w1-t1", None).unwrap_err();
    assert_eq!(err.code(), "too_large");
    assert!(matches!(
        store.append("tab-w1-t1", "x", None, None, by(NotesAuthor::User)),
        Err(NotesError::TooLarge(_))
    ));
}

#[test]
fn unsafe_keys_are_refused() {
    let dir = TempDir::new("keys");
    let mut store = NotesStore::new(dir.path());
    for key in ["", "../x", ".hidden", "a/b", "a b", &"k".repeat(129)] {
        assert!(!is_valid_key(key), "{key:?}");
        assert!(matches!(store.get(key, None), Err(NotesError::Invalid(_))));
    }
    assert!(is_valid_key(&"k".repeat(128)));
    assert!(!dir.path().exists(), "nothing is written for a bad key");
}

#[test]
fn keys_cover_tabs_long_ids_and_path_refs() {
    let tab = tab_key("W1KQ:T3");
    assert_eq!(tab, "tab-W1KQ-T3");
    assert!(is_valid_key(&tab), "uppercase public ids validate");
    assert_eq!(
        tab_key("w_1:t 2"),
        format!("tab-h{}", short_hash("w_1:t 2"))
    );

    let id = AgentSessionRef::id("6f1c2a9e-4b7d").unwrap();
    assert_eq!(session_key("claude", &id), "claude-6f1c2a9e-4b7d");

    let long = AgentSessionRef::id("a".repeat(200)).unwrap();
    let key = session_key("claude", &long);
    assert!(key.starts_with("claude-h") && key.len() == "claude-h".len() + 16);
    assert!(is_valid_key(&key));

    let path = AgentSessionRef {
        kind: AgentSessionRefKind::Path,
        value: "/Users/me/.pi/sessions/one.jsonl".into(),
    };
    let key = session_key("pi", &path);
    assert!(key.starts_with("pi-h"), "{key}");
    assert!(is_valid_key(&key));

    assert_eq!(
        key_agent_session("codex-0199aa-bb"),
        Some(("codex", "0199aa-bb"))
    );
    assert_eq!(key_agent_session(&session_key("claude", &long)), None);
    assert_eq!(key_agent_session("tab-w1-t1"), None);
}

#[test]
fn tab_notes_are_copied_to_the_agent_once_and_never_overwrite() {
    let dir = TempDir::new("migrate");
    let mut store = NotesStore::new(dir.path());
    store
        .set("tab-w1-t1", "tab notes\n", None, by(NotesAuthor::User))
        .unwrap();
    assert!(store.migrate("tab-w1-t1", "claude-s1").unwrap());
    let copied = store.get("claude-s1", None).unwrap();
    assert_eq!(copied.text.as_deref(), Some("tab notes\n"));
    assert_eq!(copied.meta.previous.as_deref(), Some("tab-w1-t1"));

    store
        .set(
            "claude-s1",
            "agent notes\n",
            Some(&copied.revision),
            by(NotesAuthor::Agent),
        )
        .unwrap();
    assert!(!store.migrate("tab-w1-t1", "claude-s1").unwrap());
    assert_eq!(
        store.get("claude-s1", None).unwrap().text.as_deref(),
        Some("agent notes\n")
    );
    assert!(!store.migrate("tab-w9-t9", "claude-s2").unwrap());
}

#[test]
fn the_first_write_records_the_previous_key() {
    let dir = TempDir::new("previous");
    let mut store = NotesStore::new(dir.path());
    let (_, written) = store
        .set(
            "claude-new",
            "x\n",
            None,
            WriteBy {
                previous_key: Some("claude-old"),
                ..by(NotesAuthor::Agent)
            },
        )
        .unwrap();
    assert_eq!(written.meta.previous.as_deref(), Some("claude-old"));
    let (_, again) = store
        .set(
            "claude-new",
            "y\n",
            Some(&written.revision),
            WriteBy {
                previous_key: Some("claude-other"),
                ..by(NotesAuthor::Agent)
            },
        )
        .unwrap();
    assert_eq!(again.meta.previous.as_deref(), Some("claude-old"));

    let mut panes = PaneKeys::default();
    assert_eq!(panes.observe("w1:p1", "claude-a"), None);
    assert_eq!(panes.observe("w1:p1", "claude-a"), None);
    assert_eq!(
        panes.observe("w1:p1", "claude-b").as_deref(),
        Some("claude-a")
    );
    assert_eq!(
        panes.observe("w1:p1", "claude-b").as_deref(),
        Some("claude-a")
    );
    panes.retain(|_| false);
    assert_eq!(panes.len(), 0);
}

#[test]
fn bounded_map_evicts_the_least_recently_used() {
    let mut map = BoundedMap::new(2);
    map.insert("a", 1);
    map.insert("b", 2);
    assert_eq!(map.get("a"), Some(&1));
    map.insert("c", 3);
    assert!(map.contains_key("a"));
    assert!(!map.contains_key("b"), "b was the oldest use");
    assert_eq!(map.insert("a", 10), Some(1));
    assert_eq!(map.len(), 2);
    assert_eq!(map.capacity(), 2);
    assert_eq!(map.peek("c"), Some(&3));
    map.retain(|key, _| *key == "c");
    assert_eq!(map.len(), 1);
    map.clear();
    assert!(map.is_empty());
}

#[test]
fn checkpoints_fold_last_wins_with_tombstones_and_anchor_patches() {
    let dir = TempDir::new("fold");
    let mut store = CheckpointStore::new(dir.path());
    let key = "claude-s1";
    let ms = 1_800_000_000_000;
    let a = store
        .add(
            key,
            new_checkpoint(CheckpointKind::Decision, "use jiff"),
            ms,
        )
        .unwrap()
        .checkpoint
        .unwrap();
    assert!(a.id.starts_with("cp_"));
    let b = store
        .add(
            key,
            new_checkpoint(CheckpointKind::Failure, "lost tz"),
            ms + 10_000,
        )
        .unwrap()
        .checkpoint
        .unwrap();
    assert_ne!(a.id, b.id);

    let updated = store
        .update(
            key,
            &a.id,
            CheckpointPatch {
                title: Some("use jiff zoned".into()),
                detail: Some("no new dependency".into()),
                ..CheckpointPatch::default()
            },
            1_800_000_100,
        )
        .unwrap();
    let updated = updated.checkpoint.unwrap();
    assert_eq!(updated.title, "use jiff zoned");
    assert_eq!(updated.ts, a.ts, "an update keeps the add's time");

    assert!(store
        .set_anchor(
            key,
            &a.id,
            AnchorRecord {
                path: Some("/tmp/t.jsonl".into()),
                offset: Some(42),
                uuid: None,
            },
            1_800_000_101,
        )
        .unwrap());
    let removed = store.remove(key, &b.id, 1_800_000_102).unwrap();
    assert!(removed.removed);
    assert!(!store
        .set_anchor(key, &b.id, AnchorRecord::default(), 1_800_000_103)
        .unwrap());
    assert!(matches!(
        store.remove(key, &b.id, 1_800_000_104),
        Err(NotesError::NotFound(_))
    ));

    let list = store.list(key, &[], None, None).unwrap();
    assert_eq!(list.checkpoints.len(), 1);
    let only = &list.checkpoints[0];
    assert_eq!(only.id, a.id);
    assert_eq!(only.title, "use jiff zoned");
    assert_eq!(only.detail.as_deref(), Some("no new dependency"));
    assert!(only.has_context);
    assert_eq!(list.started_at, Some(a.ts));

    // A torn last line does not swallow the next record.
    let path = store.path(key);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"v\":1,\"op\":\"add\"");
    fs::write(&path, bytes).unwrap();
    store
        .add(
            key,
            new_checkpoint(CheckpointKind::Note, "after tear"),
            ms + 500_000,
        )
        .unwrap();
    let titles: Vec<String> = store
        .list(key, &[], None, None)
        .unwrap()
        .checkpoints
        .into_iter()
        .map(|checkpoint| checkpoint.title)
        .collect();
    assert_eq!(titles, ["use jiff zoned", "after tear"]);
}

#[test]
fn checkpoint_list_filters_limits_and_short_circuits_on_seq() {
    let dir = TempDir::new("list");
    let mut store = CheckpointStore::new(dir.path());
    let key = "tab-w1-t1";
    let empty = store.list(key, &[], None, None).unwrap();
    assert_eq!(empty.seq, 0);
    assert!(empty.checkpoints.is_empty());
    let ms = 1_800_000_000_000;
    for (index, kind) in [
        CheckpointKind::Decision,
        CheckpointKind::Bookmark,
        CheckpointKind::Decision,
    ]
    .into_iter()
    .enumerate()
    {
        store
            .add(
                key,
                new_checkpoint(kind, &format!("cp {index}")),
                ms + index as u64 * 1000,
            )
            .unwrap();
    }
    let all = store.list(key, &[], None, None).unwrap();
    assert_eq!(all.checkpoints.len(), 3);
    assert_eq!(all.seq, fs::metadata(store.path(key)).unwrap().len());

    let unchanged = store.list(key, &[], Some(all.seq), None).unwrap();
    assert!(unchanged.unchanged);
    assert!(unchanged.checkpoints.is_empty());

    let decisions = store
        .list(key, &[CheckpointKind::Decision], None, Some(1))
        .unwrap();
    let titles: Vec<&str> = decisions
        .checkpoints
        .iter()
        .map(|checkpoint| checkpoint.title.as_str())
        .collect();
    assert_eq!(titles, ["cp 2"], "the newest of the kind");
}

#[test]
fn same_title_adds_fold_within_two_minutes_and_adds_are_rate_limited() {
    let dir = TempDir::new("rate");
    let mut store = CheckpointStore::new(dir.path());
    let key = "codex-s1";
    let ms = 1_800_000_000_000;
    let first = store
        .add(
            key,
            new_checkpoint(CheckpointKind::Milestone, "tests green"),
            ms,
        )
        .unwrap();
    assert!(!first.folded);
    let again = store
        .add(
            key,
            NewCheckpoint {
                detail: Some("macOS".into()),
                ..new_checkpoint(CheckpointKind::Milestone, "tests green")
            },
            ms + 60_000,
        )
        .unwrap();
    assert!(again.folded);
    assert_eq!(
        again.checkpoint.as_ref().unwrap().id,
        first.checkpoint.as_ref().unwrap().id
    );
    assert_eq!(again.checkpoint.unwrap().detail.as_deref(), Some("macOS"));
    let later = store
        .add(
            key,
            new_checkpoint(CheckpointKind::Milestone, "tests green"),
            ms + 200_000,
        )
        .unwrap();
    assert!(!later.folded);

    let mut at = ms + 300_000;
    for index in 2..MAX_ADDS_PER_HOUR {
        store
            .add(
                key,
                new_checkpoint(CheckpointKind::Note, &format!("n{index}")),
                at,
            )
            .unwrap();
        at += 1000;
    }
    let err = store
        .add(
            key,
            new_checkpoint(CheckpointKind::Note, "one too many"),
            at,
        )
        .unwrap_err();
    assert_eq!(err.code(), "rate_limited");
    // An hour later there is room again.
    store
        .add(
            key,
            new_checkpoint(CheckpointKind::Note, "next hour"),
            at + 3_600_000,
        )
        .unwrap();
}

#[test]
fn checkpoint_limits_are_enforced() {
    let dir = TempDir::new("limits");
    let mut store = CheckpointStore::new(dir.path());
    let key = "tab-w1-t1";
    let ms = 1_800_000_000_000;
    for new in [
        new_checkpoint(CheckpointKind::Note, "  "),
        new_checkpoint(CheckpointKind::Note, &"t".repeat(121)),
        new_checkpoint(CheckpointKind::Unknown, "x"),
        NewCheckpoint {
            detail: Some("d".repeat(2001)),
            ..new_checkpoint(CheckpointKind::Note, "x")
        },
        NewCheckpoint {
            tags: (0..9).map(|tag| tag.to_string()).collect(),
            ..new_checkpoint(CheckpointKind::Note, "x")
        },
        NewCheckpoint {
            tags: vec!["t".repeat(33)],
            ..new_checkpoint(CheckpointKind::Note, "x")
        },
    ] {
        assert!(matches!(
            store.add(key, new, ms),
            Err(NotesError::Invalid(_))
        ));
    }
    let ok = store
        .add(
            key,
            NewCheckpoint {
                tags: vec![" a ".into(), String::new()],
                ..new_checkpoint(CheckpointKind::Note, "multi\nline")
            },
            ms,
        )
        .unwrap()
        .checkpoint
        .unwrap();
    assert_eq!(ok.title, "multi line");
    assert_eq!(ok.tags, ["a"]);
}

#[test]
fn large_checkpoint_files_parse_only_their_tail() {
    let dir = TempDir::new("tail");
    let mut store = CheckpointStore::new(dir.path());
    let key = "claude-big";
    fs::create_dir_all(dir.path()).unwrap();
    let filler = format!(
        "{}\n",
        serde_json::json!({"v":1,"op":"add","id":"cp_old","ts":1,"kind":"note","title":"x".repeat(200)})
    );
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < MAX_PARSE_BYTES + 4096 {
        bytes.extend_from_slice(filler.as_bytes());
    }
    // The first record is outside the parsed tail.
    let mut file = format!(
        "{}\n",
        serde_json::json!({"v":1,"op":"add","id":"cp_first","ts":0,"kind":"decision","title":"first"})
    )
    .into_bytes();
    file.extend_from_slice(&bytes);
    file.extend_from_slice(
        format!(
            "{}\n",
            serde_json::json!({"v":1,"op":"add","id":"cp_last","ts":2,"kind":"milestone","title":"last"})
        )
        .as_bytes(),
    );
    fs::write(store.path(key), &file).unwrap();
    let list = store.list(key, &[], None, None).unwrap();
    let ids: Vec<&str> = list
        .checkpoints
        .iter()
        .map(|checkpoint| checkpoint.id.as_str())
        .collect();
    assert_eq!(ids, ["cp_old", "cp_last"]);
    assert_eq!(list.seq, file.len() as u64);
}

#[test]
fn checkpoint_ids_increase_within_a_millisecond() {
    let dir = TempDir::new("ids");
    let mut store = CheckpointStore::new(dir.path());
    let ms = 1_800_000_000_000;
    let ids: Vec<String> = (0..3)
        .map(|index| {
            store
                .add(
                    "tab-w1-t1",
                    new_checkpoint(CheckpointKind::Note, &format!("n{index}")),
                    ms,
                )
                .unwrap()
                .checkpoint
                .unwrap()
                .id
        })
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    assert_eq!(checkpoints::crockford_base32(0), "0");
    assert_eq!(checkpoints::crockford_base32(32), "10");
    assert_eq!(checkpoints::crockford_base32(31), "Z");
}

#[test]
fn worker_context_reports_missing_without_a_transcript_and_anchors_fall_back() {
    use super::worker::{anchor_at_end, run_job, ContextJob, NotesJob, NotesWorkerResult};
    use crate::api::schema::notes::CheckpointContextSource;

    let dir = TempDir::new("worker");
    let result = run_job(NotesJob::Context(ContextJob {
        key: "claude-s1".into(),
        id: "cp_1".into(),
        chars: 600,
        flavor: transcript::Flavor::Claude,
        anchor: transcript::Anchor {
            path: Some(dir.path().join("gone.jsonl")),
            offset: Some(10),
            uuid: None,
        },
        native: None,
        locate: None,
        backup_dir: Some(dir.path().join("backup")),
    }));
    let NotesWorkerResult::Context { info, located, .. } = result else {
        panic!("expected a context result");
    };
    assert_eq!(info.source, CheckpointContextSource::Missing);
    assert_eq!(info.id, "cp_1");
    assert!(located.is_none());

    fs::create_dir_all(dir.path()).unwrap();
    let file = dir.path().join("t.jsonl");
    fs::write(&file, "{\"type\":\"user\"}\n").unwrap();
    let anchor = anchor_at_end(&file, transcript::Flavor::Claude).unwrap();
    assert_eq!(anchor.path.as_deref(), Some(file.as_path()));
    assert!(anchor.offset.is_some());
    assert!(anchor_at_end(&dir.path().join("none.jsonl"), transcript::Flavor::Codex).is_none());
}

#[test]
fn the_runtime_memo_expires_misses_and_caches_contexts() {
    use super::worker::NotesWorkerResult;
    use crate::api::schema::notes::{CheckpointContextInfo, CheckpointContextSource};

    let dir = TempDir::new("runtime");
    let mut runtime = NotesRuntime::new(true, dir.path(), dir.path().join("store"));
    let now = std::time::Instant::now();
    assert_eq!(runtime.located("codex:s1", now), None);
    runtime.apply_worker_result(
        NotesWorkerResult::Located {
            memo_key: "codex:s1".into(),
            path: None,
            anchor_for: None,
            anchor: None,
        },
        now,
    );
    assert_eq!(runtime.located("codex:s1", now), Some(None));
    assert_eq!(runtime.located("codex:s1", now + LOCATE_MISS_TTL), None);

    let cache_key = NotesRuntime::context_key("claude-s1", "cp_1", 600);
    runtime.mark_context_pending(cache_key.clone(), now);
    assert!(runtime.context_pending(&cache_key, now));
    assert!(!runtime.context_pending(&cache_key, now + PENDING_TTL));
    let info = CheckpointContextInfo {
        id: "cp_1".into(),
        source: CheckpointContextSource::Native,
        prompt: Some("why".into()),
        reply: Some("because".into()),
        prompt_ts: None,
        reply_ts: None,
        continued: false,
        truncated: false,
    };
    let anchor = runtime.apply_worker_result(
        NotesWorkerResult::Context {
            key: "claude-s1".into(),
            id: "cp_1".into(),
            chars: 600,
            info: info.clone(),
            located: None,
        },
        now,
    );
    assert!(anchor.is_none());
    assert!(!runtime.context_pending(&cache_key, now));
    assert_eq!(runtime.cached_context(&cache_key), Some(info));
    runtime.forget_contexts("claude-s1", "cp_1");
    assert_eq!(runtime.cached_context(&cache_key), None);
}
