//! Store tests: temp directories only, no PTYs.

use std::path::PathBuf;

use super::*;
use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind};
use crate::api::schema::notes::{NotesAuthor, NotesWriteOutcome};

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
