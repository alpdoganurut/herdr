//! Notes recall (fork): the session's notes page handed back to its agent
//! across `/clear`, compaction and a relaunch, so it keeps what its user
//! put there (the plan, a `## Notify` preference) in view.
//!
//! - Claude: `herdr notes hook`, a `SessionStart` hook (startup, resume,
//!   clear, compact) in the wrap's `--settings` file, prints the text as
//!   `additionalContext`.
//! - Codex: the wrap adds the text to a wrapped launch's
//!   `developer_instructions` at launch (best effort: no hook re-delivers it
//!   after a Codex `/new` or compaction).
//!
//! [`fetch`] makes at most two `notes.get` calls through a caller-supplied
//! transport under one deadline; [`render`] is pure. After `/clear` (or
//! compaction) the pane's new session has no notes of its own yet: the
//! notes the pane used before (`previous`) are recalled instead. A fresh
//! start or a resume never falls back: the pane's earlier session may have
//! been unrelated work.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::schema::notes::{NotesGetParams, NotesInfo, NotesTarget};
use crate::api::schema::{Method, Request};

/// Bytes of the page recalled at most (whole lines, from the top: the page
/// starts with its title and what matters now).
pub(crate) const NOTES_BYTES: usize = 3 * 1024;
/// The whole recall budget of the hook (Claude gives the hook 5 s).
pub(crate) const HOOK_BUDGET: Duration = Duration::from_millis(2000);
/// The whole recall budget of a Codex launch (the user is waiting).
pub(crate) const LAUNCH_BUDGET: Duration = Duration::from_millis(800);
/// The first words of the recalled block.
pub(crate) const RECALL_PREFIX: &str = "[herdr+ notes]";

/// What [`fetch`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Recall {
    /// The notes key recalled.
    pub key: String,
    /// The notes are the ones the pane used before its session changed.
    pub from_previous: bool,
    pub notes: String,
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.trim().is_empty())
}

fn notes_get(
    call: &mut impl FnMut(Method) -> Result<Value, String>,
    target: NotesTarget,
) -> Result<NotesInfo, String> {
    let result = call(Method::NotesGet(NotesGetParams {
        target,
        known_revision: None,
    }))?;
    serde_json::from_value(result.get("notes").cloned().unwrap_or(Value::Null))
        .map_err(|err| err.to_string())
}

/// The pane's notes page through `call` (one API method in, its `result`
/// out). The pane's own page first; when it is empty, the pane used another
/// page before and `after_clear` (the session just started over with
/// `/clear` or compaction), that one. `None` when there is no page.
pub(crate) fn fetch(
    pane_id: &str,
    after_clear: bool,
    mut call: impl FnMut(Method) -> Result<Value, String>,
) -> Option<Recall> {
    let own = notes_get(
        &mut call,
        NotesTarget {
            pane_id: Some(pane_id.to_owned()),
            ..NotesTarget::default()
        },
    )
    .map_err(|err| tracing::debug!(event = "notes.recall", %err, "no notes"))
    .ok()?;
    if let Some(text) = non_empty(own.text) {
        return Some(Recall {
            key: own.key,
            from_previous: false,
            notes: text,
        });
    }
    let previous = own
        .previous
        .filter(|previous| after_clear && *previous != own.key)?;
    let before = notes_get(
        &mut call,
        NotesTarget {
            key: Some(previous.clone()),
            ..NotesTarget::default()
        },
    )
    .ok()?;
    non_empty(before.text).map(|text| Recall {
        key: previous,
        from_previous: true,
        notes: text,
    })
}

/// The top of `text`, whole lines, at most [`NOTES_BYTES`], and how many
/// lines were left out.
pub(crate) fn notes_head(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0;
    for line in &lines {
        if bytes + line.len() + 1 > NOTES_BYTES {
            break;
        }
        bytes += line.len() + 1;
        kept.push(line);
    }
    if kept.is_empty() {
        // One very long first line: its start.
        let first = lines.first().copied().unwrap_or_default();
        let mut cut = NOTES_BYTES.min(first.len());
        while !first.is_char_boundary(cut) {
            cut -= 1;
        }
        return (format!("{}…", &first[..cut]), lines.len().saturating_sub(1));
    }
    let left_out = lines.len() - kept.len();
    (kept.join("\n"), left_out)
}

/// The block handed to the agent. `None` when there is nothing to recall.
pub(crate) fn render(recall: &Recall) -> Option<String> {
    if recall.notes.trim().is_empty() {
        return None;
    }
    let mut out = format!(
        "{RECALL_PREFIX} Your user's notes page for this session in herdr (they see it in the info pane). It is theirs: change it only when they ask (directly or through a skill such as /plus:notes or /plus:notify); once they asked for it, keep it current when the plan or status changes."
    );
    if recall.from_previous {
        out.push_str(&format!(
            "\nIt is from this pane's earlier session (notes {}): your new session starts with no page of its own.",
            recall.key
        ));
    }
    let (head, left_out) = notes_head(&recall.notes);
    if left_out > 0 {
        out.push_str(&format!(
            "\n\n## Notes (the top; {left_out} more lines: agents_notes_read)\n"
        ));
    } else {
        out.push_str("\n\n## Notes\n");
    }
    out.push_str(&head);
    Some(crate::agent_wrap::instructions::sanitize(&out))
}

/// One API call over the local socket, within `deadline`: the `result`
/// object, or the error as text.
pub(crate) fn socket_call(method: Method, deadline: Instant) -> Result<Value, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("out of time".into());
    }
    let request = Request {
        id: "notes:recall".into(),
        method,
    };
    let mut response = crate::api::client::ApiClient::local()
        .request_value_with_timeout(&request, left)
        .map_err(|err| err.to_string())?;
    if let Some(error) = response.get("error").filter(|e| !e.is_null()) {
        return Err(format!(
            "{}: {}",
            error["code"].as_str().unwrap_or("error"),
            error["message"].as_str().unwrap_or_default()
        ));
    }
    Ok(response
        .get_mut("result")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

/// The recall text for `pane_id` over the local socket within `budget`
/// (`after_clear`: see [`fetch`]).
pub(crate) fn text_for_pane(pane_id: &str, after_clear: bool, budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    let recall = fetch(pane_id, after_clear, |method| socket_call(method, deadline))?;
    render(&recall)
}

/// The hook events that get the recall.
const HOOK_EVENTS: [&str; 1] = ["SessionStart"];

/// `SessionStart` sources after which the session starts over without its
/// notes, so the pane's earlier notes are recalled.
const CLEAR_SOURCES: [&str; 2] = ["clear", "compact"];

/// `herdr notes hook`'s stdout for Claude's hook input `stdin`: the recall
/// as `SessionStart` additional context. `text_for` gets the pane and
/// whether the session started over (`source` clear or compact). `None`
/// prints nothing (another event, no pane, nothing to recall, any failure).
pub(crate) fn hook_output(
    stdin: &str,
    pane: Option<&str>,
    text_for: impl FnOnce(&str, bool) -> Option<String>,
) -> Option<String> {
    let input: Value = serde_json::from_str(stdin).unwrap_or(Value::Null);
    let event = input["hook_event_name"].as_str().unwrap_or("SessionStart");
    if !HOOK_EVENTS.contains(&event) {
        return None;
    }
    let after_clear = input["source"]
        .as_str()
        .is_some_and(|source| CLEAR_SOURCES.contains(&source));
    let pane = pane.map(str::trim).filter(|pane| !pane.is_empty())?;
    let text = text_for(pane, after_clear)?;
    Some(
        serde_json::json!({
            "hookSpecificOutput": { "hookEventName": event, "additionalContext": text }
        })
        .to_string(),
    )
}

/// `herdr notes hook`: read Claude's hook input from stdin, print this
/// pane's recall (`$HERDR_PANE_ID`). Never fails and never blocks: every
/// error prints nothing; the caller exits 0.
pub(crate) fn run_hook() {
    use std::io::Read as _;
    let mut stdin = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut stdin);
    let pane = std::env::var("HERDR_PANE_ID").ok();
    if let Some(out) = hook_output(&stdin, pane.as_deref(), |pane, after_clear| {
        text_for_pane(pane, after_clear, HOOK_BUDGET)
    }) {
        println!("{out}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notes_json(key: &str, text: Option<&str>, previous: Option<&str>) -> Value {
        json!({ "notes": {
            "key": key, "path": format!("/n/{key}.md"), "revision": "none",
            "exists": text.is_some(), "bytes": 0, "text": text, "previous": previous,
        }})
    }

    /// A fake server: notes per key, the pane resolving to `pane_key` with
    /// `previous`. Records the calls.
    struct Fake {
        pane_key: &'static str,
        previous: Option<&'static str>,
        notes: Vec<(&'static str, &'static str)>,
        calls: Vec<String>,
    }

    impl Fake {
        fn call(&mut self, method: Method) -> Result<Value, String> {
            match method {
                Method::NotesGet(params) => {
                    let key = match (params.target.key, params.target.pane_id) {
                        (Some(key), _) => key,
                        (None, Some(_)) => self.pane_key.to_string(),
                        _ => return Err("invalid".into()),
                    };
                    self.calls.push(format!("get {key}"));
                    let text = self.notes.iter().find(|(k, _)| *k == key).map(|(_, t)| *t);
                    let previous = (key == self.pane_key).then_some(self.previous).flatten();
                    Ok(notes_json(&key, text, previous))
                }
                _ => Err("unexpected".into()),
            }
        }
    }

    #[test]
    fn the_panes_own_page_is_recalled_with_the_user_owned_rule() {
        let mut fake = Fake {
            pane_key: "claude-new",
            previous: Some("claude-old"),
            notes: vec![("claude-new", "# Plan\n- [ ] ship it")],
            calls: vec![],
        };
        let recall = fetch("w1:p1", true, |m| fake.call(m)).unwrap();
        assert_eq!(recall.key, "claude-new");
        assert!(!recall.from_previous);
        assert_eq!(fake.calls, ["get claude-new"]);
        let text = render(&recall).unwrap();
        assert!(text.starts_with(RECALL_PREFIX), "{text}");
        assert!(text.contains("## Notes\n# Plan\n- [ ] ship it"), "{text}");
        assert!(text.contains("only when they ask"), "{text}");
        assert!(!text.contains("checkpoint"), "{text}");
        assert!(!text.contains("earlier session"));
    }

    #[test]
    fn after_a_clear_the_previous_page_is_recalled() {
        let mut fake = Fake {
            pane_key: "claude-new",
            previous: Some("claude-old"),
            notes: vec![("claude-old", "remember the flag")],
            calls: vec![],
        };
        let recall = fetch("w1:p1", true, |m| fake.call(m)).unwrap();
        assert!(recall.from_previous);
        assert_eq!(recall.key, "claude-old");
        assert_eq!(recall.notes, "remember the flag");
        let text = render(&recall).unwrap();
        assert!(
            text.contains("earlier session (notes claude-old)"),
            "{text}"
        );
    }

    #[test]
    fn a_fresh_start_or_resume_never_recalls_the_panes_earlier_page() {
        let mut fake = Fake {
            pane_key: "tab-w1-1",
            previous: Some("claude-old"),
            notes: vec![("claude-old", "unrelated work")],
            calls: vec![],
        };
        assert!(fetch("w1:p1", false, |m| fake.call(m)).is_none());
        assert_eq!(fake.calls, ["get tab-w1-1"]);
        // the hook passes the start source through
        let seen = std::cell::Cell::new(None);
        for (source, after_clear) in [
            ("startup", false),
            ("resume", false),
            ("clear", true),
            ("compact", true),
        ] {
            let input = format!(r#"{{"hook_event_name":"SessionStart","source":"{source}"}}"#);
            let _ = hook_output(&input, Some("w1:p1"), |_, clear| {
                seen.set(Some(clear));
                None
            });
            assert_eq!(seen.take(), Some(after_clear), "{source}");
        }
    }

    #[test]
    fn nothing_to_recall_prints_nothing_and_failures_are_silent() {
        let mut fake = Fake {
            pane_key: "tab-w1-1",
            previous: None,
            notes: vec![("tab-w1-1", "  \n")],
            calls: vec![],
        };
        assert!(fetch("w1:p1", true, |m| fake.call(m)).is_none());
        assert!(fetch("w1:p1", true, |_| Err("notes_disabled".to_string())).is_none());
        assert!(render(&Recall::default()).is_none());
    }

    #[test]
    fn long_pages_recall_their_top_within_the_cap() {
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let (head, left_out) = notes_head(&text);
        assert!(head.len() <= NOTES_BYTES, "{}", head.len());
        assert!(head.starts_with("line 0\nline 1\n"));
        assert_eq!(head.lines().count() + left_out, 1000);
        let (head, left_out) = notes_head(&"y".repeat(NOTES_BYTES * 2));
        assert!(head.ends_with('…'));
        assert!(head.len() <= NOTES_BYTES + '…'.len_utf8());
        assert_eq!(left_out, 0);
        let (head, left_out) = notes_head("short\npage\n");
        assert_eq!((head.as_str(), left_out), ("short\npage", 0));
        let recall = Recall {
            key: "k".into(),
            from_previous: false,
            notes: text,
        };
        let out = render(&recall).unwrap();
        assert!(out.contains("more lines: agents_notes_read"), "{out}");
    }

    #[test]
    fn the_hook_answers_session_start_only_with_a_pane() {
        let input = r#"{"hook_event_name":"SessionStart","source":"clear","session_id":"x"}"#;
        let out = hook_output(input, Some("w1:p1"), |pane, _| {
            assert_eq!(pane, "w1:p1");
            Some("recalled".into())
        })
        .unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(value["hookSpecificOutput"]["additionalContext"], "recalled");
        assert!(hook_output(input, None, |_, _| Some("x".into())).is_none());
        assert!(hook_output(input, Some(" "), |_, _| Some("x".into())).is_none());
        assert!(hook_output(
            r#"{"hook_event_name":"UserPromptSubmit"}"#,
            Some("w1:p1"),
            |_, _| Some("x".into())
        )
        .is_none());
        assert!(hook_output(input, Some("w1:p1"), |_, _| None).is_none());
        // unreadable input: treated as a session start
        assert!(hook_output("garbage", Some("w1:p1"), |_, _| Some("x".into())).is_some());
    }
}
