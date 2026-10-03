//! Notes recall (fork): the session's notes and recent checkpoints handed
//! back to its agent, so they work as the agent's own memory across
//! `/clear`, compaction and a relaunch.
//!
//! - Claude: `herdr notes hook`, a `SessionStart` hook (startup, resume,
//!   clear, compact) in the wrap's `--settings` file, prints the text as
//!   `additionalContext`.
//! - Codex: the wrap adds the text to a wrapped launch's
//!   `developer_instructions` at launch (best effort: no hook re-delivers it
//!   after a Codex `/new` or compaction).
//!
//! [`fetch`] makes at most four `notes.get` / `checkpoints.list` calls
//! through a caller-supplied transport under one deadline; [`render`] is
//! pure. After `/clear` (or compaction) the pane's new session has no notes
//! of its own yet: the notes the pane used before (`previous`) are recalled
//! instead. A fresh start or a resume never falls back: the pane's earlier
//! session may have been unrelated work.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::schema::notes::{
    CheckpointInfo, CheckpointKind, CheckpointsListInfo, CheckpointsListParams, NotesGetParams,
    NotesInfo, NotesTarget,
};
use crate::api::schema::{Method, Request};

/// The tag automatic checkpoints carry (`[notes] auto_checkpoints`).
pub(crate) const AUTO_TAG: &str = "auto";
/// Lines of notes recalled (the end of the notes: the running log's tail).
pub(crate) const NOTES_LINES: usize = 60;
/// Bytes of notes recalled at most (whole lines, from the end).
pub(crate) const NOTES_BYTES: usize = 6 * 1024;
/// Checkpoints recalled.
pub(crate) const CHECKPOINTS: usize = 8;
/// Checkpoints read to pick them from (automatic prompt bookmarks skipped).
const CHECKPOINTS_READ: u32 = 40;
/// Characters of a checkpoint's detail in its recalled line.
const DETAIL_CHARS: usize = 160;
/// The whole recall budget of the hook (Claude gives the hook 5 s).
pub(crate) const HOOK_BUDGET: Duration = Duration::from_millis(2000);
/// The whole recall budget of a Codex launch (the user is waiting).
pub(crate) const LAUNCH_BUDGET: Duration = Duration::from_millis(800);
/// The first words of the recalled block.
pub(crate) const RECALL_PREFIX: &str = "[herdr+ notes]";

/// Whether a checkpoint is one herdr added on its own.
pub(crate) fn is_auto(checkpoint: &CheckpointInfo) -> bool {
    checkpoint.tags.iter().any(|tag| tag == AUTO_TAG)
}

/// What [`fetch`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Recall {
    /// The notes key recalled.
    pub key: String,
    /// The notes are the ones the pane used before its session changed.
    pub from_previous: bool,
    pub notes: Option<String>,
    /// Oldest first.
    pub checkpoints: Vec<CheckpointInfo>,
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

fn checkpoints_of(
    call: &mut impl FnMut(Method) -> Result<Value, String>,
    key: &str,
) -> Vec<CheckpointInfo> {
    let result = call(Method::CheckpointsList(CheckpointsListParams {
        target: NotesTarget {
            key: Some(key.to_owned()),
            ..NotesTarget::default()
        },
        kinds: Vec::new(),
        since_seq: None,
        limit: Some(CHECKPOINTS_READ),
    }));
    let list: Option<CheckpointsListInfo> = result
        .ok()
        .and_then(|result| serde_json::from_value(result.get("checkpoints")?.clone()).ok());
    pick_checkpoints(list.map(|list| list.checkpoints).unwrap_or_default())
}

/// The newest [`CHECKPOINTS`], oldest first, without herdr's automatic
/// prompt bookmarks (the agent knows its own prompts; commits and failures
/// stay).
pub(crate) fn pick_checkpoints(all: Vec<CheckpointInfo>) -> Vec<CheckpointInfo> {
    let kept: Vec<CheckpointInfo> = all
        .into_iter()
        .filter(|cp| !(is_auto(cp) && cp.kind == CheckpointKind::Bookmark))
        .collect();
    let skip = kept.len().saturating_sub(CHECKPOINTS);
    kept.into_iter().skip(skip).collect()
}

/// The pane's notes and checkpoints through `call` (one API method in, its
/// `result` out). The pane's own notes first; when they are empty, the pane
/// used other notes before and `after_clear` (the session just started over
/// with `/clear` or compaction), those.
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
    let previous = own
        .previous
        .clone()
        .filter(|previous| after_clear && *previous != own.key);
    let mut recall = Recall {
        key: own.key.clone(),
        from_previous: false,
        notes: non_empty(own.text),
        checkpoints: checkpoints_of(&mut call, &own.key),
    };
    if let (None, Some(previous)) = (&recall.notes, previous) {
        if let Ok(before) = notes_get(
            &mut call,
            NotesTarget {
                key: Some(previous.clone()),
                ..NotesTarget::default()
            },
        ) {
            if let Some(text) = non_empty(before.text) {
                recall.notes = Some(text);
                recall.from_previous = true;
                recall.key = previous.clone();
                if recall.checkpoints.is_empty() {
                    recall.checkpoints = checkpoints_of(&mut call, &previous);
                }
            }
        }
    }
    (recall.notes.is_some() || !recall.checkpoints.is_empty()).then_some(recall)
}

/// The last [`NOTES_LINES`] lines of `text`, at most [`NOTES_BYTES`], and
/// how many lines were left out.
fn notes_tail(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().rev().take(NOTES_LINES) {
        if bytes + line.len() + 1 > NOTES_BYTES && !kept.is_empty() {
            break;
        }
        bytes += line.len() + 1;
        kept.push(line);
    }
    kept.reverse();
    let mut tail = kept.join("\n");
    if tail.len() > NOTES_BYTES {
        // One very long line: its end.
        let mut cut = tail.len() - NOTES_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = format!("…{}", &tail[cut..]);
    }
    (tail, lines.len() - kept.len())
}

fn kind_name(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Decision => "decision",
        CheckpointKind::Milestone => "milestone",
        CheckpointKind::Failure => "failure",
        CheckpointKind::Bookmark => "bookmark",
        CheckpointKind::Note | CheckpointKind::Unknown => "note",
    }
}

fn one_line(text: &str, max: usize) -> String {
    crate::coordinator::one_line(text, max)
}

/// The block handed to the agent; `clock` formats a checkpoint's time.
/// `None` when there is nothing to recall.
pub(crate) fn render(recall: &Recall, clock: impl Fn(u64) -> String) -> Option<String> {
    if recall.notes.is_none() && recall.checkpoints.is_empty() {
        return None;
    }
    let mut out = format!(
        "{RECALL_PREFIX} Your notes and recent checkpoints in herdr (your memory; your user sees them in the info pane). Pick up from them and keep them current: agents_checkpoint for decisions, milestones, failures and before you stop; agents_notes_append section=Log for the running log."
    );
    if recall.from_previous {
        out.push_str(&format!(
            "\nThey are from this pane's earlier session (notes {}): your new session starts with empty notes.",
            recall.key
        ));
    }
    if let Some(text) = &recall.notes {
        let (tail, left_out) = notes_tail(text);
        if left_out > 0 {
            out.push_str(&format!(
                "\n\n## Notes (the last part; {left_out} earlier lines: agents_notes_read)\n"
            ));
        } else {
            out.push_str("\n\n## Notes\n");
        }
        out.push_str(&tail);
    }
    if !recall.checkpoints.is_empty() {
        out.push_str("\n\n## Recent checkpoints (oldest first; more: agents_checkpoints_list)");
        for cp in &recall.checkpoints {
            let mut line = format!(
                "\n- {} {}{}: {}",
                clock(cp.ts),
                kind_name(cp.kind),
                if is_auto(cp) { " (auto)" } else { "" },
                one_line(&cp.title, 120)
            );
            if let Some(detail) = cp.detail.as_deref().filter(|d| !d.trim().is_empty()) {
                line.push_str(" — ");
                line.push_str(&one_line(detail, DETAIL_CHARS));
            }
            out.push_str(&line);
        }
    }
    Some(crate::agent_wrap::instructions::sanitize(&out))
}

/// `HH:MM` of `ts` at `offset_seconds` from UTC.
pub(crate) fn hhmm(ts: u64, offset_seconds: i64) -> String {
    let local = i64::try_from(ts)
        .unwrap_or(0)
        .saturating_add(offset_seconds);
    let day = local.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, day / 60 % 60)
}

/// This machine's UTC offset in whole minutes (0 without a local clock).
pub(crate) fn local_offset_seconds() -> i64 {
    crate::platform::local_datetime()
        .map(|local| {
            let utc = time::OffsetDateTime::now_utc();
            let utc = time::PrimitiveDateTime::new(utc.date(), utc.time());
            ((local - utc).whole_seconds() + 30).div_euclid(60) * 60
        })
        .unwrap_or(0)
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
    let offset = local_offset_seconds();
    render(&recall, |ts| hhmm(ts, offset))
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
    use crate::api::schema::notes::NotesAuthor;
    use serde_json::json;

    fn cp(id: &str, kind: CheckpointKind, title: &str, auto: bool) -> CheckpointInfo {
        CheckpointInfo {
            id: id.into(),
            ts: 3600 * 14 + 60 * 2,
            kind,
            author: NotesAuthor::Agent,
            title: title.into(),
            detail: None,
            tags: if auto { vec![AUTO_TAG.into()] } else { vec![] },
            has_context: false,
        }
    }

    fn notes_json(key: &str, text: Option<&str>, previous: Option<&str>) -> Value {
        json!({ "notes": {
            "key": key, "path": format!("/n/{key}.md"), "revision": "none",
            "exists": text.is_some(), "bytes": 0, "text": text, "previous": previous,
        }})
    }

    fn list_json(key: &str, cps: &[CheckpointInfo]) -> Value {
        json!({ "checkpoints": { "key": key, "seq": 1, "unchanged": false, "checkpoints": cps } })
    }

    /// A fake server: notes and checkpoints per key, the pane resolving to
    /// `pane_key` with `previous`. Records the calls.
    struct Fake {
        pane_key: &'static str,
        previous: Option<&'static str>,
        notes: Vec<(&'static str, &'static str)>,
        cps: Vec<(&'static str, Vec<CheckpointInfo>)>,
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
                Method::CheckpointsList(params) => {
                    let key = params.target.key.unwrap_or_default();
                    self.calls.push(format!("list {key}"));
                    let cps = self
                        .cps
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, c)| c.clone())
                        .unwrap_or_default();
                    Ok(list_json(&key, &cps))
                }
                _ => Err("unexpected".into()),
            }
        }
    }

    #[test]
    fn the_panes_own_notes_and_checkpoints_are_recalled() {
        let mut fake = Fake {
            pane_key: "claude-new",
            previous: Some("claude-old"),
            notes: vec![("claude-new", "## Log\n- did a thing")],
            cps: vec![(
                "claude-new",
                vec![cp("a", CheckpointKind::Decision, "use sqlite", false)],
            )],
            calls: vec![],
        };
        let recall = fetch("w1:p1", true, |m| fake.call(m)).unwrap();
        assert_eq!(recall.key, "claude-new");
        assert!(!recall.from_previous);
        assert_eq!(fake.calls, ["get claude-new", "list claude-new"]);
        let text = render(&recall, |ts| hhmm(ts, 0)).unwrap();
        assert!(text.starts_with(RECALL_PREFIX), "{text}");
        assert!(text.contains("## Notes\n## Log\n- did a thing"), "{text}");
        assert!(text.contains("- 14:02 decision: use sqlite"), "{text}");
        assert!(!text.contains("earlier session"));
    }

    #[test]
    fn after_a_clear_the_previous_notes_are_recalled_whichever_key_the_server_has() {
        // The server already moved the pane to the new session: its notes
        // are empty and `previous` names the old key.
        let mut fake = Fake {
            pane_key: "claude-new",
            previous: Some("claude-old"),
            notes: vec![("claude-old", "remember the flag")],
            cps: vec![(
                "claude-old",
                vec![cp("a", CheckpointKind::Milestone, "parser done", false)],
            )],
            calls: vec![],
        };
        let recall = fetch("w1:p1", true, |m| fake.call(m)).unwrap();
        assert!(recall.from_previous);
        assert_eq!(recall.key, "claude-old");
        assert_eq!(recall.notes.as_deref(), Some("remember the flag"));
        assert_eq!(recall.checkpoints.len(), 1);
        let text = render(&recall, |ts| hhmm(ts, 0)).unwrap();
        assert!(
            text.contains("earlier session (notes claude-old)"),
            "{text}"
        );
        // The server has not seen the new session yet: the pane still
        // resolves to the old key, which is recalled directly.
        let mut fake = Fake {
            pane_key: "claude-old",
            previous: None,
            notes: vec![("claude-old", "remember the flag")],
            cps: vec![],
            calls: vec![],
        };
        let recall = fetch("w1:p1", true, |m| fake.call(m)).unwrap();
        assert!(!recall.from_previous);
        assert_eq!(recall.notes.as_deref(), Some("remember the flag"));
    }

    #[test]
    fn a_fresh_start_or_resume_never_recalls_the_panes_earlier_notes() {
        // A new `claude` in a pane that ran another task: its own notes are
        // empty and `previous` names that unrelated session.
        let mut fake = Fake {
            pane_key: "tab-w1-1",
            previous: Some("claude-old"),
            notes: vec![("claude-old", "unrelated work")],
            cps: vec![(
                "claude-old",
                vec![cp("a", CheckpointKind::Milestone, "old task", false)],
            )],
            calls: vec![],
        };
        assert!(fetch("w1:p1", false, |m| fake.call(m)).is_none());
        assert_eq!(fake.calls, ["get tab-w1-1", "list tab-w1-1"]);
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
            notes: vec![],
            cps: vec![],
            calls: vec![],
        };
        assert!(fetch("w1:p1", true, |m| fake.call(m)).is_none());
        assert!(fetch("w1:p1", true, |_| Err("notes_disabled".to_string())).is_none());
        assert!(render(&Recall::default(), |_| String::new()).is_none());
    }

    #[test]
    fn automatic_prompt_bookmarks_are_not_recalled_and_eight_at_most() {
        let mut all: Vec<CheckpointInfo> = (0..12)
            .map(|i| {
                cp(
                    &format!("c{i}"),
                    CheckpointKind::Note,
                    &format!("n{i}"),
                    false,
                )
            })
            .collect();
        all.push(cp("p", CheckpointKind::Bookmark, "you: 14:02", true));
        all.push(cp("m", CheckpointKind::Milestone, "committed abc", true));
        let picked = pick_checkpoints(all);
        assert_eq!(picked.len(), CHECKPOINTS);
        assert_eq!(picked.last().map(|c| c.id.as_str()), Some("m"));
        assert!(picked.iter().all(|c| c.id != "p"));
        assert_eq!(picked[0].id, "c5");
        let text = render(
            &Recall {
                key: "k".into(),
                checkpoints: picked,
                ..Recall::default()
            },
            |_| "t".into(),
        )
        .unwrap();
        assert!(
            text.contains("- t milestone (auto): committed abc"),
            "{text}"
        );
    }

    #[test]
    fn long_notes_recall_their_end_within_the_caps() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let (tail, left_out) = notes_tail(&text);
        assert_eq!(tail.lines().count(), NOTES_LINES);
        assert_eq!(left_out, 40);
        assert!(tail.ends_with("line 99"));
        let wide: String = (0..30)
            .map(|i| format!("{i} {}\n", "x".repeat(400)))
            .collect();
        let (tail, left_out) = notes_tail(&wide);
        assert!(tail.len() <= NOTES_BYTES, "{}", tail.len());
        assert!(left_out > 0);
        let (tail, _) = notes_tail(&"y".repeat(NOTES_BYTES * 2));
        assert!(tail.starts_with('…'));
        let recall = Recall {
            key: "k".into(),
            notes: Some(text),
            ..Recall::default()
        };
        let out = render(&recall, |_| String::new()).unwrap();
        assert!(out.contains("40 earlier lines: agents_notes_read"), "{out}");
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

    #[test]
    fn times_are_local_clock_minutes() {
        assert_eq!(hhmm(3600 * 14 + 120, 0), "14:02");
        assert_eq!(hhmm(3600 * 14 + 120, 3600 * 3), "17:02");
        assert_eq!(hhmm(60, -3600), "23:01");
    }
}
