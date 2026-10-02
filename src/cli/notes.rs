//! `herdr notes` and `herdr checkpoint` (fork): the per-session notes and the
//! checkpoint timeline behind the info pane (`notes.get|set|append`,
//! `checkpoints.list|add|update|remove|context`).
//!
//! Inside a herdr pane the target is that pane (its agent session's notes,
//! else its tab's). Elsewhere name one with `--tab ID`, `--pane ID` or
//! `--key K`.

use std::io::Read;
use std::time::{Duration, Instant};

use crate::api::schema::notes::{
    CheckpointContextInfo, CheckpointContextSource, CheckpointInfo, CheckpointKind,
    CheckpointTarget, CheckpointWriteInfo, CheckpointsAddParams, CheckpointsContextParams,
    CheckpointsListInfo, CheckpointsListParams, CheckpointsUpdateParams, NotesAppendParams,
    NotesAuthor, NotesGetParams, NotesInfo, NotesSetParams, NotesTarget, NotesWriteInfo,
    NotesWriteOutcome,
};
use crate::api::schema::{Method, Request};

const NOTES_USAGE: &str = "usage: herdr notes <read [--json]|path|append [TEXT|-] [--section S] [--stamp]|write --base REV [--file F | -]> [--tab ID|--pane ID|--key K]";
const CHECKPOINT_USAGE: &str = "usage: herdr checkpoint <add KIND TITLE [--detail D] [--tag T]... [--as user]|list [--kind K] [--limit N] [--json]|show ID [--chars N]|rm ID|edit ID [--title T] [--detail D] [--kind K] [--tag T]...> [--tab ID|--pane ID|--key K]";
const KINDS: &str = "decision, milestone, failure, bookmark or note";
/// How long `checkpoint show` waits for a context that is still being read.
const SHOW_WAIT: Duration = Duration::from_secs(5);
const SHOW_POLL: Duration = Duration::from_millis(250);
/// Exit code of `notes write` on a conflict.
const EXIT_CONFLICT: i32 = 3;

/// Parsed arguments: positionals plus `--name value` options and flags.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    positional: Vec<String>,
    options: Vec<(String, String)>,
    flags: Vec<String>,
}

impl Args {
    fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.options
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .collect()
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    fn target(&self) -> Option<NotesTarget> {
        let target = NotesTarget {
            tab_id: self.option("tab").map(str::to_owned),
            pane_id: self.option("pane").map(str::to_owned),
            key: self.option("key").map(str::to_owned),
        };
        if target != NotesTarget::default() {
            return Some(target);
        }
        super::target::caller_pane_id().map(|pane_id| NotesTarget {
            pane_id: Some(pane_id),
            ..NotesTarget::default()
        })
    }
}

/// Split `args` given which options take a value and which are flags.
fn parse(args: &[String], valued: &[&str], flags: &[&str]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        let Some(name) = arg.strip_prefix("--").filter(|name| !name.is_empty()) else {
            parsed.positional.push(arg.clone());
            continue;
        };
        let (name, inline) = match name.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (name, None),
        };
        if flags.contains(&name) {
            if inline.is_some() {
                return Err(format!("--{name} takes no value"));
            }
            parsed.flags.push(name.to_owned());
        } else if valued.contains(&name) {
            let value = match inline {
                Some(value) => value,
                None => {
                    let value = args
                        .get(index)
                        .cloned()
                        .ok_or_else(|| format!("missing value for --{name}"))?;
                    index += 1;
                    value
                }
            };
            parsed.options.push((name.to_owned(), value));
        } else {
            return Err(format!("unknown option --{name}"));
        }
    }
    Ok(parsed)
}

const TARGET_OPTIONS: [&str; 3] = ["tab", "pane", "key"];

fn with_target<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut valued: Vec<&str> = TARGET_OPTIONS.to_vec();
    valued.extend_from_slice(extra);
    valued
}

fn is_help(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
}

fn usage(text: &str, code: i32) -> std::io::Result<i32> {
    eprintln!("{text}");
    Ok(code)
}

fn missing_target() -> std::io::Result<i32> {
    eprintln!("herdr: not inside a herdr pane; name the notes with --tab ID, --pane ID or --key K");
    Ok(2)
}

/// Send one request; an error reply is printed and becomes `Err(exit)`.
fn call(id: &str, method: Method) -> std::io::Result<Result<serde_json::Value, i32>> {
    let response = super::send_request(&Request {
        id: id.into(),
        method,
    })?;
    if let Some(error) = response.get("error") {
        let code = error
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("error");
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        eprintln!("herdr: {code}: {message}");
        return Ok(Err(1));
    }
    Ok(Ok(response))
}

fn result_field<T: serde::de::DeserializeOwned>(
    response: &serde_json::Value,
    pointer: &str,
) -> std::io::Result<T> {
    let value = response
        .pointer(pointer)
        .cloned()
        .ok_or_else(|| std::io::Error::other(format!("the server sent no {pointer}")))?;
    serde_json::from_value(value).map_err(std::io::Error::other)
}

fn read_stdin() -> std::io::Result<String> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(text)
}

pub(super) fn run_notes_command(args: &[String]) -> std::io::Result<i32> {
    let Some(verb) = args.first().map(String::as_str) else {
        return usage(NOTES_USAGE, 2);
    };
    let rest = &args[1..];
    if matches!(verb, "help" | "--help" | "-h") {
        return usage(NOTES_USAGE, 0);
    }
    if is_help(rest) {
        return usage(NOTES_USAGE, 0);
    }
    match verb {
        "read" => notes_read(rest),
        "path" => notes_path(rest),
        "append" => notes_append(rest),
        "write" => notes_write(rest),
        _ => usage(NOTES_USAGE, 2),
    }
}

fn notes_get(target: NotesTarget) -> std::io::Result<Result<NotesInfo, i32>> {
    let response = match call(
        "cli:notes:get",
        Method::NotesGet(NotesGetParams {
            target,
            known_revision: None,
        }),
    )? {
        Ok(response) => response,
        Err(code) => return Ok(Err(code)),
    };
    result_field(&response, "/result/notes").map(Ok)
}

fn notes_read(args: &[String]) -> std::io::Result<i32> {
    let args = match parse(args, &with_target(&[]), &["json"]) {
        Ok(args) if args.positional.is_empty() => args,
        Ok(_) => return usage("usage: herdr notes read [--json]", 2),
        Err(err) => return usage(&err, 2),
    };
    let Some(target) = args.target() else {
        return missing_target();
    };
    let notes = match notes_get(target)? {
        Ok(notes) => notes,
        Err(code) => return Ok(code),
    };
    if args.flag("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&notes).map_err(std::io::Error::other)?
        );
        return Ok(0);
    }
    eprintln!("{} rev {} ({})", notes.key, notes.revision, notes.path);
    let text = notes.text.unwrap_or_default();
    print!("{text}");
    if !text.is_empty() && !text.ends_with('\n') {
        println!();
    }
    Ok(0)
}

fn notes_path(args: &[String]) -> std::io::Result<i32> {
    let args = match parse(args, &with_target(&[]), &[]) {
        Ok(args) if args.positional.is_empty() => args,
        Ok(_) => return usage("usage: herdr notes path", 2),
        Err(err) => return usage(&err, 2),
    };
    let Some(target) = args.target() else {
        // Outside a pane: where every notes file lives.
        println!("{}", crate::notes::notes_dir().display());
        return Ok(0);
    };
    match notes_get(target)? {
        Ok(notes) => {
            println!("{}", notes.path);
            Ok(0)
        }
        Err(code) => Ok(code),
    }
}

fn print_write(write: &NotesWriteInfo) -> i32 {
    match write.outcome {
        NotesWriteOutcome::Written => {
            println!("rev {}", write.notes.revision);
            0
        }
        NotesWriteOutcome::Unchanged => {
            println!("unchanged rev {}", write.notes.revision);
            0
        }
        NotesWriteOutcome::Conflict => {
            println!("conflict");
            eprintln!(
                "herdr: the notes changed (now rev {}); read them again and retry with --base {}",
                write.notes.revision, write.notes.revision
            );
            EXIT_CONFLICT
        }
        NotesWriteOutcome::Unknown => {
            println!("rev {}", write.notes.revision);
            0
        }
    }
}

fn notes_append(args: &[String]) -> std::io::Result<i32> {
    let args = match parse(args, &with_target(&["section"]), &["stamp"]) {
        Ok(args) if args.positional.len() <= 1 => args,
        Ok(_) => {
            return usage(
                "usage: herdr notes append [TEXT|-] [--section S] [--stamp] (quote TEXT)",
                2,
            )
        }
        Err(err) => return usage(&err, 2),
    };
    let Some(target) = args.target() else {
        return missing_target();
    };
    let text = match args.positional.first().map(String::as_str) {
        None | Some("-") => read_stdin()?,
        Some(text) => text.to_owned(),
    };
    let response = match call(
        "cli:notes:append",
        Method::NotesAppend(NotesAppendParams {
            target,
            text,
            section: args.option("section").map(str::to_owned),
            stamp: args.flag("stamp"),
            author: NotesAuthor::Agent,
        }),
    )? {
        Ok(response) => response,
        Err(code) => return Ok(code),
    };
    let write: NotesWriteInfo = result_field(&response, "/result/write")?;
    Ok(print_write(&write))
}

fn notes_write(args: &[String]) -> std::io::Result<i32> {
    const WRITE_USAGE: &str = "usage: herdr notes write --base REV [--file F | -]";
    let args = match parse(args, &with_target(&["base", "file"]), &[]) {
        Ok(args) => args,
        Err(err) => return usage(&err, 2),
    };
    let Some(base) = args.option("base").map(str::to_owned) else {
        return usage(WRITE_USAGE, 2);
    };
    let text = match (args.option("file"), args.positional.as_slice()) {
        (Some(file), []) => std::fs::read_to_string(file)?,
        (None, []) => read_stdin()?,
        (None, [dash]) if dash == "-" => read_stdin()?,
        _ => return usage(WRITE_USAGE, 2),
    };
    let Some(target) = args.target() else {
        return missing_target();
    };
    let response = match call(
        "cli:notes:set",
        Method::NotesSet(NotesSetParams {
            target,
            text,
            base_revision: Some(base),
            author: NotesAuthor::Agent,
        }),
    )? {
        Ok(response) => response,
        Err(code) => return Ok(code),
    };
    let write: NotesWriteInfo = result_field(&response, "/result/write")?;
    Ok(print_write(&write))
}

fn parse_kind(value: &str) -> Option<CheckpointKind> {
    match value {
        "decision" => Some(CheckpointKind::Decision),
        "milestone" => Some(CheckpointKind::Milestone),
        "failure" => Some(CheckpointKind::Failure),
        "bookmark" => Some(CheckpointKind::Bookmark),
        "note" => Some(CheckpointKind::Note),
        _ => None,
    }
}

fn kind_name(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Decision => "decision",
        CheckpointKind::Milestone => "milestone",
        CheckpointKind::Failure => "failure",
        CheckpointKind::Bookmark => "bookmark",
        CheckpointKind::Note => "note",
        CheckpointKind::Unknown => "unknown",
    }
}

pub(super) fn run_checkpoint_command(args: &[String]) -> std::io::Result<i32> {
    let Some(verb) = args.first().map(String::as_str) else {
        return usage(CHECKPOINT_USAGE, 2);
    };
    let rest = &args[1..];
    if matches!(verb, "help" | "--help" | "-h") || is_help(rest) {
        return usage(CHECKPOINT_USAGE, 0);
    }
    match verb {
        "add" => checkpoint_add(rest),
        "list" | "ls" => checkpoint_list(rest),
        "show" => checkpoint_show(rest),
        "rm" | "remove" => checkpoint_rm(rest),
        "edit" => checkpoint_edit(rest),
        _ => usage(CHECKPOINT_USAGE, 2),
    }
}

fn print_checkpoint_write(write: &CheckpointWriteInfo) {
    let Some(checkpoint) = &write.checkpoint else {
        return;
    };
    if write.removed {
        println!("removed {}", checkpoint.id);
    } else if write.folded {
        println!("{} (updated the same checkpoint)", checkpoint.id);
    } else {
        println!("{}", checkpoint.id);
    }
}

fn checkpoint_add(args: &[String]) -> std::io::Result<i32> {
    const ADD_USAGE: &str =
        "usage: herdr checkpoint add <decision|milestone|failure|bookmark|note> TITLE [--detail D] [--tag T]... [--as user]";
    let args = match parse(args, &with_target(&["detail", "tag", "as"]), &[]) {
        Ok(args) => args,
        Err(err) => return usage(&err, 2),
    };
    let [kind, title @ ..] = args.positional.as_slice() else {
        return usage(ADD_USAGE, 2);
    };
    if title.is_empty() {
        return usage(ADD_USAGE, 2);
    }
    let Some(kind) = parse_kind(kind) else {
        eprintln!("herdr: unknown checkpoint kind {kind:?}; use {KINDS}");
        return Ok(2);
    };
    let author = match args.option("as") {
        None | Some("agent") => NotesAuthor::Agent,
        Some("user") => NotesAuthor::User,
        Some(other) => {
            eprintln!("herdr: --as takes user or agent (got {other:?})");
            return Ok(2);
        }
    };
    let Some(target) = args.target() else {
        return missing_target();
    };
    let response = match call(
        "cli:checkpoints:add",
        Method::CheckpointsAdd(CheckpointsAddParams {
            target,
            kind,
            title: title.join(" "),
            detail: args.option("detail").map(str::to_owned),
            tags: args.all("tag"),
            author,
        }),
    )? {
        Ok(response) => response,
        Err(code) => return Ok(code),
    };
    let write: CheckpointWriteInfo = result_field(&response, "/result/checkpoint")?;
    print_checkpoint_write(&write);
    Ok(0)
}

fn checkpoint_list(args: &[String]) -> std::io::Result<i32> {
    let args = match parse(args, &with_target(&["kind", "limit"]), &["json"]) {
        Ok(args) if args.positional.is_empty() => args,
        Ok(_) => {
            return usage(
                "usage: herdr checkpoint list [--kind K] [--limit N] [--json]",
                2,
            )
        }
        Err(err) => return usage(&err, 2),
    };
    let mut kinds = Vec::new();
    for value in args.all("kind") {
        match parse_kind(&value) {
            Some(kind) => kinds.push(kind),
            None => {
                eprintln!("herdr: unknown checkpoint kind {value:?}; use {KINDS}");
                return Ok(2);
            }
        }
    }
    let limit = match args.option("limit").map(str::parse::<u32>) {
        None => None,
        Some(Ok(limit)) if limit > 0 => Some(limit),
        Some(_) => {
            eprintln!("herdr: --limit takes a positive number");
            return Ok(2);
        }
    };
    let Some(target) = args.target() else {
        return missing_target();
    };
    let response = match call(
        "cli:checkpoints:list",
        Method::CheckpointsList(CheckpointsListParams {
            target,
            kinds,
            since_seq: None,
            limit,
        }),
    )? {
        Ok(response) => response,
        Err(code) => return Ok(code),
    };
    let list: CheckpointsListInfo = result_field(&response, "/result/checkpoints")?;
    if args.flag("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&list).map_err(std::io::Error::other)?
        );
        return Ok(0);
    }
    if list.checkpoints.is_empty() {
        eprintln!("no checkpoints for {}", list.key);
        return Ok(0);
    }
    let clock = Clock::now();
    for checkpoint in &list.checkpoints {
        println!("{}", list_line(checkpoint, &clock));
    }
    Ok(0)
}

fn list_line(checkpoint: &CheckpointInfo, clock: &Clock) -> String {
    let author = match checkpoint.author {
        NotesAuthor::User => "user",
        _ => "agent",
    };
    let mut line = format!(
        "{}  {}  {:<9}  {:<5}  {}",
        checkpoint.id,
        clock.format(checkpoint.ts),
        kind_name(checkpoint.kind),
        author,
        checkpoint.title
    );
    if !checkpoint.tags.is_empty() {
        line.push_str(&format!("  [{}]", checkpoint.tags.join(", ")));
    }
    line
}

fn checkpoint_show(args: &[String]) -> std::io::Result<i32> {
    let args = match parse(args, &with_target(&["chars"]), &["json"]) {
        Ok(args) => args,
        Err(err) => return usage(&err, 2),
    };
    let [id] = args.positional.as_slice() else {
        return usage("usage: herdr checkpoint show ID [--chars N]", 2);
    };
    let chars = match args.option("chars").map(str::parse::<u32>) {
        None => None,
        Some(Ok(chars)) if chars > 0 => Some(chars),
        Some(_) => {
            eprintln!("herdr: --chars takes a positive number");
            return Ok(2);
        }
    };
    let Some(target) = args.target() else {
        return missing_target();
    };
    let list = match call(
        "cli:checkpoints:list",
        Method::CheckpointsList(CheckpointsListParams {
            target: target.clone(),
            ..CheckpointsListParams::default()
        }),
    )? {
        Ok(response) => result_field::<CheckpointsListInfo>(&response, "/result/checkpoints")?,
        Err(code) => return Ok(code),
    };
    let Some(checkpoint) = list
        .checkpoints
        .into_iter()
        .find(|checkpoint| checkpoint.id == *id)
    else {
        eprintln!("herdr: no checkpoint {id:?}");
        return Ok(1);
    };
    let deadline = Instant::now() + SHOW_WAIT;
    let context = loop {
        let response = match call(
            "cli:checkpoints:context",
            Method::CheckpointsContext(CheckpointsContextParams {
                target: target.clone(),
                id: id.clone(),
                chars,
            }),
        )? {
            Ok(response) => response,
            Err(code) => return Ok(code),
        };
        let context: CheckpointContextInfo = result_field(&response, "/result/context")?;
        if context.source != CheckpointContextSource::Pending || Instant::now() >= deadline {
            break context;
        }
        std::thread::sleep(SHOW_POLL);
    };
    if args.flag("json") {
        let both = serde_json::json!({ "checkpoint": checkpoint, "context": context });
        println!(
            "{}",
            serde_json::to_string_pretty(&both).map_err(std::io::Error::other)?
        );
        return Ok(0);
    }
    print!("{}", show_text(&checkpoint, &context, &Clock::now()));
    Ok(0)
}

fn show_text(
    checkpoint: &CheckpointInfo,
    context: &CheckpointContextInfo,
    clock: &Clock,
) -> String {
    let mut out = format!(
        "{} {} · {} · {}\n",
        kind_name(checkpoint.kind),
        checkpoint.id,
        clock.format(checkpoint.ts),
        match checkpoint.author {
            NotesAuthor::User => "user",
            _ => "agent",
        }
    );
    out.push_str(&format!("{}\n", checkpoint.title));
    if let Some(detail) = &checkpoint.detail {
        out.push_str(&format!("\n{detail}\n"));
    }
    if !checkpoint.tags.is_empty() {
        out.push_str(&format!("tags: {}\n", checkpoint.tags.join(", ")));
    }
    let source = match context.source {
        CheckpointContextSource::Native => "transcript",
        CheckpointContextSource::Backup => "herdr backup",
        CheckpointContextSource::BackupPrevious => "previous herdr backup",
        CheckpointContextSource::Pending => "still reading; try again",
        CheckpointContextSource::Missing => "transcript not found",
        CheckpointContextSource::Unsupported => "no transcript context",
        CheckpointContextSource::Unknown => "unknown source",
    };
    out.push_str(&format!("\ncontext: {source}\n"));
    if context.continued {
        out.push_str("(the prompt is earlier in the session)\n");
    }
    if let Some(prompt) = &context.prompt {
        out.push_str(&format!("\n> prompt\n{prompt}\n"));
    }
    if let Some(reply) = &context.reply {
        out.push_str(&format!("\n< reply\n{reply}\n"));
    }
    if context.truncated {
        out.push_str("(cut; pass --chars N for more)\n");
    }
    out
}

fn checkpoint_rm(args: &[String]) -> std::io::Result<i32> {
    let args = match parse(args, &with_target(&[]), &[]) {
        Ok(args) => args,
        Err(err) => return usage(&err, 2),
    };
    let [id] = args.positional.as_slice() else {
        return usage("usage: herdr checkpoint rm ID", 2);
    };
    let Some(target) = args.target() else {
        return missing_target();
    };
    let response = match call(
        "cli:checkpoints:remove",
        Method::CheckpointsRemove(CheckpointTarget {
            target,
            id: id.clone(),
        }),
    )? {
        Ok(response) => response,
        Err(code) => return Ok(code),
    };
    let write: CheckpointWriteInfo = result_field(&response, "/result/checkpoint")?;
    print_checkpoint_write(&write);
    Ok(0)
}

fn checkpoint_edit(args: &[String]) -> std::io::Result<i32> {
    const EDIT_USAGE: &str =
        "usage: herdr checkpoint edit ID [--title T] [--detail D] [--kind K] [--tag T]...";
    let args = match parse(args, &with_target(&["title", "detail", "kind", "tag"]), &[]) {
        Ok(args) => args,
        Err(err) => return usage(&err, 2),
    };
    let [id] = args.positional.as_slice() else {
        return usage(EDIT_USAGE, 2);
    };
    let kind = match args.option("kind") {
        None => None,
        Some(value) => match parse_kind(value) {
            Some(kind) => Some(kind),
            None => {
                eprintln!("herdr: unknown checkpoint kind {value:?}; use {KINDS}");
                return Ok(2);
            }
        },
    };
    let tags = args.all("tag");
    let params = CheckpointsUpdateParams {
        target: NotesTarget::default(),
        id: id.clone(),
        kind,
        title: args.option("title").map(str::to_owned),
        detail: args.option("detail").map(str::to_owned),
        tags: (!tags.is_empty()).then_some(tags),
    };
    if params.kind.is_none()
        && params.title.is_none()
        && params.detail.is_none()
        && params.tags.is_none()
    {
        return usage(EDIT_USAGE, 2);
    }
    let Some(target) = args.target() else {
        return missing_target();
    };
    let response = match call(
        "cli:checkpoints:update",
        Method::CheckpointsUpdate(CheckpointsUpdateParams { target, ..params }),
    )? {
        Ok(response) => response,
        Err(code) => return Ok(code),
    };
    let write: CheckpointWriteInfo = result_field(&response, "/result/checkpoint")?;
    print_checkpoint_write(&write);
    Ok(0)
}

/// Local wall-clock formatting for checkpoint times.
struct Clock {
    offset_seconds: i64,
}

impl Clock {
    fn now() -> Self {
        let offset_seconds = crate::platform::local_datetime()
            .map(|local| {
                let utc = time::OffsetDateTime::now_utc();
                let utc = time::PrimitiveDateTime::new(utc.date(), utc.time());
                // Whole minutes: the two clocks are read a fraction apart.
                ((local - utc).whole_seconds() + 30).div_euclid(60) * 60
            })
            .unwrap_or(0);
        Self { offset_seconds }
    }

    /// `YYYY-MM-DD HH:MM` local.
    fn format(&self, ts: u64) -> String {
        let local = i64::try_from(ts).unwrap_or(i64::MAX / 2) + self.offset_seconds;
        match time::OffsetDateTime::from_unix_timestamp(local) {
            Ok(at) => format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                at.year(),
                u8::from(at.month()),
                at.day(),
                at.hour(),
                at.minute()
            ),
            Err(_) => ts.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn options_flags_and_positionals_parse() {
        let args = parse(
            &strings(&[
                "decision", "Use", "jiff", "--tag", "a", "--tag=b", "--detail", "why", "--tab",
                "w1:t2",
            ]),
            &with_target(&["detail", "tag"]),
            &["json"],
        )
        .unwrap();
        assert_eq!(args.positional, strings(&["decision", "Use", "jiff"]));
        assert_eq!(args.all("tag"), strings(&["a", "b"]));
        assert_eq!(args.option("detail"), Some("why"));
        assert_eq!(
            args.target(),
            Some(NotesTarget {
                tab_id: Some("w1:t2".into()),
                ..NotesTarget::default()
            })
        );
        assert!(parse(&strings(&["--nope"]), &[], &[]).is_err());
        assert!(parse(&strings(&["--tag"]), &["tag"], &[]).is_err());
        assert!(parse(&strings(&["--json=1"]), &[], &["json"]).is_err());
        assert!(parse(&strings(&["--json"]), &[], &["json"])
            .unwrap()
            .flag("json"));
    }

    #[test]
    fn kinds_round_trip_and_lines_render() {
        for name in ["decision", "milestone", "failure", "bookmark", "note"] {
            assert_eq!(kind_name(parse_kind(name).unwrap()), name);
        }
        assert_eq!(parse_kind("Decision"), None);
        let clock = Clock { offset_seconds: 0 };
        let checkpoint = CheckpointInfo {
            id: "cp_1".into(),
            ts: 0,
            kind: CheckpointKind::Decision,
            author: NotesAuthor::User,
            title: "use jiff".into(),
            detail: Some("no new dependency".into()),
            tags: vec!["design".into()],
            has_context: true,
        };
        assert_eq!(
            list_line(&checkpoint, &clock),
            "cp_1  1970-01-01 00:00  decision   user   use jiff  [design]"
        );
        let context = CheckpointContextInfo {
            id: "cp_1".into(),
            source: CheckpointContextSource::Native,
            prompt: Some("pick the fix".into()),
            reply: Some("zoned".into()),
            prompt_ts: None,
            reply_ts: None,
            continued: false,
            truncated: true,
        };
        let text = show_text(&checkpoint, &context, &clock);
        assert!(text.contains("> prompt\npick the fix\n"), "{text}");
        assert!(text.contains("< reply\nzoned\n"));
        assert!(text.contains("context: transcript"));
        assert!(text.contains("--chars"));
    }

    #[test]
    fn conflicts_exit_three() {
        let write = NotesWriteInfo {
            outcome: NotesWriteOutcome::Conflict,
            notes: NotesInfo {
                key: "k".into(),
                revision: "sha256:abc".into(),
                ..NotesInfo::default()
            },
        };
        assert_eq!(print_write(&write), EXIT_CONFLICT);
        let written = NotesWriteInfo {
            outcome: NotesWriteOutcome::Written,
            ..write
        };
        assert_eq!(print_write(&written), 0);
    }
}
