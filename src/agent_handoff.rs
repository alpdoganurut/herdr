//! Agent handoff, then compact (fork): before a Claude agent's context is
//! compacted, herdr has it write a handoff for its future self, saves the
//! transcript, types `/compact`, and points the compacted session back at
//! both. Named `agent_handoff` because "handoff" alone is herdr's live
//! server handoff.
//!
//! This module is pure: the per-pane state machine ([`Run`], [`next`]), the
//! `[agents.handoff]` settings resolved for one run, the prompt texts
//! (`{path}` templating, the default prompt, the resume line, the
//! SessionStart(compact) injection), the file layout under
//! `<state_dir>/handoffs/` and the mechanical handoff herdr writes when the
//! agent did not. The file work runs on a worker thread
//! ([`files`]); the App glue (triggers, typing, hooks) is
//! `src/app/agent_handoff.rs`.
//!
//! The flow of one run:
//!
//! ```text
//! AwaitPrompt --free--> type the prompt --> AwaitHandoff
//! AwaitHandoff --turn ended after the prompt + handoff.md non-empty--> AwaitCompact
//! AwaitHandoff --turn ended, no file, grace--> one reminder line --> AwaitHandoff
//! AwaitHandoff --no file after the reminder (grace) or timeout-->
//!     WritingFallback (mechanical handoff) --> AwaitCompact
//! AwaitCompact --free--> type /compact --> Compacting
//! Compacting --SessionStart(compact) or an idle edge--> AwaitResume | done
//! AwaitResume --free--> type the resume line --> done
//! ```
//!
//! A compaction herdr did not start (Claude's auto-compact, the user's own
//! `/compact`) reports PreCompact first: herdr saves the transcript and a
//! mechanical handoff and the run starts at `Compacting`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) mod files;

/// `[agents.handoff] at_percent` default.
pub(crate) const DEFAULT_AT_PERCENT: u8 = 80;
/// How long the agent has to write its handoff after the prompt went in.
pub(crate) const HANDOFF_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// After a turn end that left no handoff file: how long herdr waits for a
/// later turn end before it reminds the agent once (then, after the same
/// wait again, writes the mechanical handoff).
pub(crate) const MISSING_GRACE: Duration = Duration::from_secs(90);
/// How long the handoff turn may read blocked (a permission prompt, a
/// question) before the user is told the run waits on it.
pub(crate) const BLOCKED_NOTICE: Duration = Duration::from_secs(2 * 60);
/// How long a run waits for the agent to be free before it gives up.
pub(crate) const FREE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long `/compact` may take before the run gives up on it.
pub(crate) const COMPACT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A detection idle edge counts as the end of `/compact` only this long
/// after it was typed (the command's first frames can read idle).
pub(crate) const COMPACT_MIN: Duration = Duration::from_secs(3);
/// The directory under the state dir.
pub(crate) const DIR_NAME: &str = "handoffs";
pub(crate) const HANDOFF_FILE: &str = "handoff.md";
pub(crate) const TRANSCRIPT_FILE: &str = "transcript.jsonl";
pub(crate) const META_FILE: &str = "meta.json";
pub(crate) const LATEST_FILE: &str = "latest.txt";
/// The first words of the SessionStart(compact) injection.
pub(crate) const INJECTION_PREFIX: &str = "[herdr+ handoff]";

/// An agent herdr can hand off and compact. Claude only for now; Codex has
/// `/compact` too, but no PreCompact / SessionStart(compact) hooks yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandoffAgent {
    Claude,
}

impl HandoffAgent {
    pub(crate) fn from_agent(agent: crate::detect::Agent) -> Option<Self> {
        match agent {
            crate::detect::Agent::Claude => Some(Self::Claude),
            _ => None,
        }
    }

    /// What is typed to compact the session.
    pub(crate) fn compact_command(self) -> &'static str {
        match self {
            Self::Claude => "/compact",
        }
    }
}

/// What started a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// The context use reached `at_percent`.
    Auto { percent: u8 },
    /// `agents.handoff` (the tab menu, the keybinding, the CLI, an agent).
    Manual { by: String },
    /// A compaction herdr did not start (PreCompact): `auto` is Claude's own
    /// auto-compact (mid-turn; it continues the turn by itself).
    External { auto: bool },
}

impl Trigger {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Auto { .. } => "auto",
            Self::Manual { .. } => "manual",
            Self::External { auto: true } => "external_auto",
            Self::External { auto: false } => "external_manual",
        }
    }

    /// Whether the resume line is typed after the compaction: not after
    /// Claude's own mid-turn auto-compact, whose turn carries on.
    pub(crate) fn types_resume(&self) -> bool {
        !matches!(self, Self::External { auto: true })
    }
}

/// Who wrote the handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The agent, as asked.
    Agent,
    /// herdr, from what it knows (the agent did not, or was not asked).
    Mechanical,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Mechanical => "mechanical",
        }
    }
}

/// Where a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The run's files are being set up (the transcript copy) or the agent
    /// is not free yet: the prompt goes in once it is.
    AwaitPrompt,
    /// The prompt (or the reminder) was typed at `typed_at`; `checked` is
    /// the turn end the last file check was for (each end is checked once),
    /// `checking` while a check is on the worker. The phase's own start
    /// (the prompt) bounds it by [`HANDOFF_TIMEOUT`].
    AwaitHandoff {
        typed_at: Instant,
        checked: Option<Instant>,
        checking: bool,
    },
    /// herdr's own handoff is being written.
    WritingFallback,
    /// The handoff is saved: `/compact` goes in once the agent is free.
    AwaitCompact,
    /// `/compact` was typed at `typed_at` (or PreCompact reported one herdr
    /// did not start).
    Compacting { typed_at: Instant },
    /// Compacted: the resume line goes in once the agent is free.
    AwaitResume,
}

impl Phase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::AwaitPrompt => "await_prompt",
            Self::AwaitHandoff { .. } => "await_handoff",
            Self::WritingFallback => "writing_fallback",
            Self::AwaitCompact => "await_compact",
            Self::Compacting { .. } => "compacting",
            Self::AwaitResume => "await_resume",
        }
    }
}

/// The `[agents.handoff]` settings one run uses (resolved when it starts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunSettings {
    /// The custom prompt line (`{path}` replaced), `None` for herdr's own.
    pub(crate) command: Option<String>,
    pub(crate) resume: bool,
}

/// One pane's run (runtime only: a restart or a live handoff forgets it).
#[derive(Debug, Clone)]
pub(crate) struct Run {
    /// Distinguishes worker results of an older run.
    pub(crate) id: u64,
    pub(crate) agent: HandoffAgent,
    pub(crate) trigger: Trigger,
    pub(crate) settings: RunSettings,
    pub(crate) phase: Phase,
    /// When the run entered its phase.
    pub(crate) phase_since: Instant,
    pub(crate) started_unix: u64,
    /// The agent's session id the files are filed under.
    pub(crate) session_id: String,
    pub(crate) paths: RunPaths,
    /// The transcript copy is done (`Some(true)`), failed or had no source
    /// (`Some(false)`), or still on the worker (`None`).
    pub(crate) transcript_saved: Option<bool>,
    /// Who wrote the handoff, once it is saved.
    pub(crate) kind: Option<Kind>,
    /// Since when the agent has been free without a break (the settle).
    pub(crate) free_since: Option<Instant>,
    /// Failed writes in a row (a refusal other than the typing guard).
    pub(crate) write_failures: u32,
    /// Since when a turn end after the prompt found no handoff file (cleared
    /// by the reminder).
    pub(crate) missing_since: Option<Instant>,
    /// The reminder line was typed.
    pub(crate) reminded: bool,
    /// Since when the handoff turn has read blocked without a break.
    pub(crate) blocked_since: Option<Instant>,
    /// The user was told the handoff turn waits on a prompt.
    pub(crate) blocked_noted: bool,
}

impl Run {
    pub(crate) fn enter(&mut self, phase: Phase, now: Instant) {
        self.phase = phase;
        self.phase_since = now;
        self.free_since = None;
    }

    /// Whether queued agent messages wait for this run: from the handoff
    /// prompt until the resume line, so nothing lands between the handoff
    /// turn and `/compact`, or in the compaction.
    pub(crate) fn holds_messages(&self) -> bool {
        !matches!(self.phase, Phase::AwaitPrompt)
    }
}

/// What the App sees of the pane for one step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Facts {
    pub(crate) now: Instant,
    /// The agent can take typed input and has for the settle time.
    pub(crate) free: bool,
    /// The agent's last turn end its Stop hook reported.
    pub(crate) hook_end: Option<Instant>,
    /// The last detection idle edge.
    pub(crate) idle_edge: Option<Instant>,
}

/// What the App does next for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    Wait,
    /// Type the handoff prompt.
    TypePrompt,
    /// Stat the handoff file on the worker (the turn that ended at `end`).
    CheckFile {
        end: Instant,
    },
    /// The turn ended without the file: remind the agent once.
    TypeReminder,
    /// The agent did not write in time: herdr writes the handoff.
    WriteMechanical,
    TypeCompact,
    /// `/compact` ended without a SessionStart(compact) report (no hooks):
    /// treat the idle edge as its end.
    Compacted,
    TypeResume,
    /// Stop the run (the reason is logged and shown).
    Abort(String),
    /// Nothing left to do (the resume line was not typed in time).
    Finish(String),
}

/// The next step of `run` (pure). Called by the server tick about once a
/// second while the pane has a run.
pub(crate) fn next(run: &Run, facts: &Facts) -> Effect {
    let waited = facts.now.saturating_duration_since(run.phase_since);
    match run.phase {
        Phase::AwaitPrompt => {
            if run.transcript_saved.is_none() {
                // The prompt names files the copy is creating.
                return Effect::Wait;
            }
            if facts.free {
                Effect::TypePrompt
            } else if waited >= FREE_TIMEOUT {
                Effect::Abort("the agent was never free for the handoff prompt".into())
            } else {
                Effect::Wait
            }
        }
        Phase::AwaitHandoff {
            typed_at,
            checked,
            checking,
        } => {
            if checking {
                return Effect::Wait;
            }
            if waited >= HANDOFF_TIMEOUT {
                return Effect::WriteMechanical;
            }
            // The turn the prompt started ended: its Stop hook reported an
            // end after the prompt, or (no hooks) the screen went idle after
            // it and stayed so for the settle.
            let hook_end = facts.hook_end.filter(|end| *end > typed_at);
            let idle_end = facts
                .idle_edge
                .filter(|edge| *edge > typed_at && facts.free);
            if let Some(end) = hook_end.or(idle_end) {
                if checked.is_none_or(|checked| checked < end) {
                    return Effect::CheckFile { end };
                }
            }
            // A turn ended without the file and no later end came in the
            // grace: remind the agent once, then write herdr's own.
            match run.missing_since {
                Some(since) if facts.now.saturating_duration_since(since) >= MISSING_GRACE => {
                    if run.reminded {
                        Effect::WriteMechanical
                    } else if facts.free {
                        Effect::TypeReminder
                    } else {
                        Effect::Wait
                    }
                }
                _ => Effect::Wait,
            }
        }
        Phase::WritingFallback => Effect::Wait,
        Phase::AwaitCompact => {
            if facts.free {
                Effect::TypeCompact
            } else if waited >= FREE_TIMEOUT {
                Effect::Abort("the agent was never free for /compact".into())
            } else {
                Effect::Wait
            }
        }
        Phase::Compacting { typed_at } => {
            if facts
                .idle_edge
                .is_some_and(|edge| edge > typed_at + COMPACT_MIN)
                && facts.free
            {
                Effect::Compacted
            } else if waited >= COMPACT_TIMEOUT {
                match run.trigger {
                    // Not herdr's compaction: no report of its end is not a
                    // failure the user needs a card for.
                    Trigger::External { .. } => {
                        Effect::Finish("no report of the compaction's end".into())
                    }
                    _ => Effect::Abort("/compact did not finish in time".into()),
                }
            } else {
                Effect::Wait
            }
        }
        Phase::AwaitResume => {
            if facts.free {
                Effect::TypeResume
            } else if waited >= FREE_TIMEOUT {
                Effect::Finish("the agent was busy; no resume line".into())
            } else {
                Effect::Wait
            }
        }
    }
}

/// The phase after a compaction ended (its SessionStart(compact) report or
/// the idle fallback): the resume line when the run types one, else done
/// (`None`).
pub(crate) fn after_compaction(run: &Run) -> Option<Phase> {
    (run.settings.resume && run.trigger.types_resume()).then_some(Phase::AwaitResume)
}

/// The auto trigger's arming (pure): whether a read of `percent` starts a
/// run, and the new armed flag. Fires once per compaction cycle: disarmed
/// when it fires, armed again once a read shows the use below the
/// threshold. A read over it while a run is on (a manual one) disarms too:
/// that run's compaction is this cycle's. `at_percent` 0 never fires.
pub(crate) fn auto_fire(armed: bool, percent: u8, at_percent: u8, running: bool) -> (bool, bool) {
    if at_percent == 0 || percent < at_percent {
        return (false, true);
    }
    (armed && !running, false)
}

// ----- texts ------------------------------------------------------------------

/// A custom prompt line with `{path}` replaced; a line without the
/// placeholder gets the path appended.
pub(crate) fn render_command(template: &str, path: &Path) -> String {
    let path = path.display().to_string();
    let template = template.trim();
    if template.contains("{path}") {
        template.replace("{path}", &path)
    } else {
        format!("{template} {path}")
    }
}

/// herdr's own handoff prompt (one line, typed into the agent).
pub(crate) fn default_prompt(path: &Path, percent: Option<u8>) -> String {
    let why = match percent {
        Some(percent) => format!("your context is {percent}% full, so it is compacted next"),
        None => "your context is compacted next".to_string(),
    };
    format!(
        "herdr: {why}. First write a handoff for your future self to {}: the current state, the decisions made and why, dead ends and gotchas, the files in play, and the exact next step. Write only that file, then end your turn; herdr runs /compact after it and points you back to the file.",
        path.display()
    )
}

/// The prompt a run types.
pub(crate) fn prompt(run: &Run, percent: Option<u8>) -> String {
    match &run.settings.command {
        Some(template) => render_command(template, &run.paths.handoff),
        None => default_prompt(&run.paths.handoff, percent),
    }
}

/// The reminder typed when the handoff turn ended without the file.
pub(crate) fn reminder_line(handoff: &Path) -> String {
    format!(
        "herdr: the handoff file is still missing at {}. Write it now (the current state, decisions and why, dead ends, files in play, the exact next step), then end your turn.",
        handoff.display()
    )
}

/// The line typed after the compaction, so the agent starts a turn.
pub(crate) fn resume_line(handoff: &Path, transcript: &Path) -> String {
    format!(
        "herdr: compacted \u{b7} handoff {} \u{b7} transcript {} \u{2014} read the handoff and continue (or pause if nothing is active)",
        handoff.display(),
        transcript.display()
    )
}

/// The SessionStart(compact) hook's additional context.
pub(crate) fn injection(handoff: &Path, transcript: Option<&Path>, kind: Kind) -> String {
    let mut text = format!(
        "{INJECTION_PREFIX} This session was just compacted. Before it, herdr saved a handoff at {}",
        handoff.display()
    );
    if kind == Kind::Mechanical {
        text.push_str(" (written by herdr from your notes, checkpoints, git state and recent prompts, not by you)");
    }
    if let Some(transcript) = transcript {
        text.push_str(&format!(
            " and the full pre-compaction transcript at {}",
            transcript.display()
        ));
    }
    text.push_str(". Read the handoff first, then continue the active work; if there is no active work, stop and wait for your user.");
    text
}

// ----- files ------------------------------------------------------------------

/// One run's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunPaths {
    /// `<root>/<session>/<stamp>/`.
    pub(crate) dir: PathBuf,
    pub(crate) handoff: PathBuf,
    pub(crate) transcript: PathBuf,
    pub(crate) meta: PathBuf,
    /// `<root>/<session>/latest.txt`.
    pub(crate) latest: PathBuf,
}

/// `<state_dir>/handoffs`.
pub(crate) fn root(state_dir: &Path) -> PathBuf {
    state_dir.join(DIR_NAME)
}

/// A session id as a directory name: ASCII letters, digits, `-`, `_` and
/// `.` (not leading), at most 128; anything else is `unknown`.
pub(crate) fn session_dir_name(session_id: &str) -> String {
    let id = session_id.trim();
    let valid = !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if valid {
        id.to_string()
    } else {
        "unknown".to_string()
    }
}

/// `20261013T101500Z` for a unix time (UTC).
pub(crate) fn utc_stamp(unix: u64) -> String {
    let at = i64::try_from(unix)
        .ok()
        .and_then(|unix| time::OffsetDateTime::from_unix_timestamp(unix).ok())
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// The files of a run started at `unix` for `session_id`.
pub(crate) fn run_paths(root: &Path, session_id: &str, unix: u64) -> RunPaths {
    let session = root.join(session_dir_name(session_id));
    let dir = session.join(utc_stamp(unix));
    RunPaths {
        handoff: dir.join(HANDOFF_FILE),
        transcript: dir.join(TRANSCRIPT_FILE),
        meta: dir.join(META_FILE),
        latest: session.join(LATEST_FILE),
        dir,
    }
}

// ----- the mechanical handoff -----------------------------------------------------

/// What herdr knows for its own handoff (gathered on the server, the git
/// and transcript parts read on the worker).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Mechanical {
    pub(crate) agent_name: String,
    pub(crate) pane: String,
    pub(crate) trigger: String,
    /// Why herdr wrote it.
    pub(crate) reason: String,
    pub(crate) written_unix: u64,
    pub(crate) cwd: Option<PathBuf>,
    /// The tail of the agent's herdr notes.
    pub(crate) notes_tail: Option<String>,
    /// Recent checkpoints, oldest first, one line each.
    pub(crate) checkpoints: Vec<String>,
    /// `git status --porcelain=v1 -b` lines (branch first).
    pub(crate) git: Vec<String>,
    /// The last user prompts, oldest first.
    pub(crate) prompts: Vec<String>,
    pub(crate) transcript: Option<PathBuf>,
}

/// Lines of notes kept in a mechanical handoff.
pub(crate) const MECHANICAL_NOTES_LINES: usize = 40;
/// Prompts kept.
pub(crate) const MECHANICAL_PROMPTS: usize = 5;
/// Characters per prompt.
pub(crate) const MECHANICAL_PROMPT_CHARS: usize = 400;
/// git status lines kept.
pub(crate) const MECHANICAL_GIT_LINES: usize = 40;

/// The last `max` lines of `text`.
pub(crate) fn tail_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let skip = lines.len().saturating_sub(max);
    lines[skip..].join("\n")
}

/// The mechanical handoff as markdown.
pub(crate) fn render_mechanical(m: &Mechanical) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Handoff (written by herdr, {})\n\n",
        utc_stamp(m.written_unix)
    ));
    out.push_str(&format!(
        "herdr wrote this for {} (pane {}) before compacting: {}. It is a mechanical summary of what herdr knows; the agent's own reasoning is in the transcript.\n",
        m.agent_name, m.pane, m.reason
    ));
    out.push_str(&format!("\nTrigger: {}\n", m.trigger));
    if let Some(cwd) = &m.cwd {
        out.push_str(&format!("Working directory: {}\n", cwd.display()));
    }
    if let Some(transcript) = &m.transcript {
        out.push_str(&format!(
            "Full transcript (pre-compaction): {}\n",
            transcript.display()
        ));
    }
    out.push_str("\n## Recent prompts (oldest first)\n");
    if m.prompts.is_empty() {
        out.push_str("- (none found)\n");
    }
    for prompt in &m.prompts {
        out.push_str(&format!("- {}\n", prompt.replace('\n', " ")));
    }
    out.push_str("\n## Git\n");
    if m.git.is_empty() {
        out.push_str("- (not a git repository, or git failed)\n");
    } else {
        out.push_str("```\n");
        for line in &m.git {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("```\n");
    }
    out.push_str("\n## Recent checkpoints (oldest first)\n");
    if m.checkpoints.is_empty() {
        out.push_str("- (none)\n");
    }
    for checkpoint in &m.checkpoints {
        out.push_str(&format!("- {checkpoint}\n"));
    }
    out.push_str("\n## Notes (the end)\n");
    match m
        .notes_tail
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        Some(text) => {
            out.push_str(text.trim_end());
            out.push('\n');
        }
        None => out.push_str("(no notes)\n"),
    }
    out.push_str("\n## Next step\nRead the recent prompts and the transcript's end to find where the work stopped; if nothing is active, wait for your user.\n");
    out
}

/// A one-line text cut to `max` characters.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

/// Up to `max` of the last user prompts in a Claude transcript's text
/// (JSON lines), oldest first: `type` `user` lines whose message content is
/// typed text (not tool results, meta lines, compaction summaries, slash
/// commands or herdr's own lines).
pub(crate) fn last_user_prompts(jsonl: &str, max: usize) -> Vec<String> {
    let mut prompts: Vec<String> = Vec::new();
    for line in jsonl.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["type"] != "user" || value["isMeta"] == true || value["isCompactSummary"] == true {
            continue;
        }
        let content = &value["message"]["content"];
        let text = match content {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Array(parts) => {
                let texts: Vec<&str> = parts
                    .iter()
                    .filter(|part| part["type"] == "text")
                    .filter_map(|part| part["text"].as_str())
                    .collect();
                (!texts.is_empty()).then(|| texts.join(" "))
            }
            _ => None,
        };
        let Some(text) = text else {
            continue;
        };
        let text = text.trim();
        if text.is_empty()
            || is_claude_wrapper(text)
            || text.starts_with('/')
            || text.starts_with("herdr:")
        {
            continue;
        }
        prompts.push(one_line(text, MECHANICAL_PROMPT_CHARS));
    }
    let skip = prompts.len().saturating_sub(max);
    prompts.split_off(skip)
}

/// Whether a user line is one of Claude Code's own wrappers (a slash
/// command's echo, a local command's output, a reminder, a `!` shell line, a
/// task notification), not text the user typed. A prompt that merely starts
/// with markup (`<div>`) is kept.
fn is_claude_wrapper(text: &str) -> bool {
    const WRAPPERS: [&str; 6] = [
        "<command-",
        "<local-command-",
        "<system-reminder",
        "<bash-",
        "<task-notification",
        "<user-prompt-submit-hook",
    ];
    WRAPPERS.iter().any(|prefix| text.starts_with(prefix))
}

#[cfg(test)]
mod tests;
