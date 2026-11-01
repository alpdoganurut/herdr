//! Typing guard (fork): automatic typing (an agent message, a coordinator
//! wake-up) never lands in a pane while its user is typing there or has an
//! unsent draft in the agent's input box.
//!
//! Two independent checks, either one blocks:
//! - recent user input: a client sent key, text or paste input to the pane
//!   within [`USER_INPUT_QUIET`] (the stamp lives on the terminal runtime and
//!   is written by the server's client input path only; API writes such as
//!   `pane send-text` or `agent prompt` never stamp);
//! - an unsent draft: the bottom of the agent's screen, read with its styles,
//!   shows non-placeholder text in the input box. Claude Code and Codex are
//!   recognised; every other agent relies on the recent-input check alone.
//!
//! Evidence (Claude Code 2.1.288, Codex 0.160.0, `agent read --source recent
//! --format ansi`): both draw their placeholder faint (SGR 2, `❯ ⟨2m⟩Try "…"`,
//! `› ⟨2m⟩Ask Codex to do anything`) and the user's text in the default
//! style; both use the hardware cursor (no inverse cell), focused or not.
//! Claude's box is the text between the last two `─` rules; Codex's composer
//! is the last `›` line plus its continuation lines up to the blank line
//! above the footer.
//!
//! The same classification ([`prompt_box`]) marks the input box in an
//! agents_read ([`annotate_prompt_box`]): plain text loses the faint style,
//! so a suggestion would read like something the user typed.
//!
//! Fail closed for Claude Code and Codex: automatic typing goes in only
//! when the input box is on the screen, its first visible cell is the
//! prompt marker and nothing but faint text follows it. A dialog, a
//! permission prompt, a plan approval or a menu that replaces or hides the
//! box ([`TypingBlock::InputBoxAbsent`]) holds it like a draft would: the
//! pointer line's characters and its Enter would otherwise answer the
//! dialog (Claude Code 2.1.299 captures in `typing_guard/`).

use std::ops::Range;
use std::time::{Duration, Instant};

use crate::detect::Agent;

/// Tests: a screen whose Claude Code box (between the last two rules) and
/// Codex composer (the last `›` line) are both free. Without an input box
/// the guard holds automatic typing (fails closed), so a test runtime that
/// stands in for an idle agent draws this.
#[cfg(test)]
pub(crate) const TEST_FREE_INPUT_BOX: &[u8] = "\r\n─────\r\n❯ \r\n─────\r\n› ".as_bytes();

/// How long after the user's last keystroke in a pane automatic typing waits.
pub(crate) const USER_INPUT_QUIET: Duration = Duration::from_secs(10);

/// Why automatic typing is held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypingBlock {
    /// The user typed into the pane within [`USER_INPUT_QUIET`].
    RecentInput,
    /// The agent's input box holds text the user has not sent.
    UnsentDraft,
    /// The agent is in a voice session (live or muted, src/app/voice.rs).
    VoiceMode,
    /// The agent's input box is not on its screen, or something other than
    /// the bare prompt marker leads it: a dialog, a permission prompt or a
    /// menu is shown (Claude Code and Codex only).
    InputBoxAbsent,
}

impl TypingBlock {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            TypingBlock::RecentInput => "the user is typing in it",
            TypingBlock::UnsentDraft => "the user has an unsent draft in its input box",
            TypingBlock::VoiceMode => "it is in voice mode",
            TypingBlock::InputBoxAbsent => "its input box is not on screen (a dialog or a menu?)",
        }
    }

    /// The `agent.prompt` error code a guarded prompt is refused with:
    /// `input_box_absent` for a missing input box, else `user_typing`.
    pub(crate) fn error_code(self) -> &'static str {
        match self {
            TypingBlock::InputBoxAbsent => "input_box_absent",
            TypingBlock::RecentInput | TypingBlock::UnsentDraft | TypingBlock::VoiceMode => {
                "user_typing"
            }
        }
    }

    /// Whether the block is the user's own typing (recent input, a draft,
    /// voice), not a missing input box.
    pub(crate) fn is_user_typing(self) -> bool {
        !matches!(self, TypingBlock::InputBoxAbsent)
    }
}

/// The guard's decision. `bottom_ansi` is only read when the recent-input
/// check passes and the agent's input box is recognised. Fails closed for
/// agents whose box it knows ([`composer`]).
pub(crate) fn typing_block(
    agent: Agent,
    last_user_input: Option<Instant>,
    now: Instant,
    bottom_ansi: impl FnOnce() -> String,
) -> Option<TypingBlock> {
    if last_user_input.is_some_and(|at| now.saturating_duration_since(at) < USER_INPUT_QUIET) {
        return Some(TypingBlock::RecentInput);
    }
    if !draft_detection_supported(agent) {
        return None;
    }
    match composer(agent, &bottom_ansi()) {
        Composer::Free => None,
        Composer::Draft => Some(TypingBlock::UnsentDraft),
        Composer::Absent => Some(TypingBlock::InputBoxAbsent),
    }
}

/// What the screen offers automatic typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Composer {
    /// The input box is on the screen, led by its prompt marker, with no
    /// text the user typed: typing may go in.
    Free,
    /// The input box, led by its marker, holds the user's unsent text.
    Draft,
    /// No input box led by its marker: a dialog, a menu or an unknown
    /// screen. Never typed into.
    Absent,
}

/// The fail-closed composer check behind [`typing_block`]: `Free` only when
/// the agent's box is located, its first visible cell is the prompt marker
/// and only faint text (a placeholder or suggestion) follows it.
pub(crate) fn composer(agent: Agent, ansi: &str) -> Composer {
    let lines: Vec<Vec<Cell>> = ansi.lines().map(parse_styled_line).collect();
    let Some((body, marker)) = locate_box(agent, &lines) else {
        return Composer::Absent;
    };
    let (prompt, marker_seen) = classify(&lines[body], marker);
    match (prompt, marker_seen) {
        (PromptBox::Empty | PromptBox::Suggestion(_), true) => Composer::Free,
        (PromptBox::Draft { .. }, true) => Composer::Draft,
        (_, false) => Composer::Absent,
    }
}

/// Fork (lost-message fix, src/app/message_queue.rs): what the input box
/// holds now when it is on screen and led by its prompt marker; `None`
/// otherwise (no box, a dialog, a menu, an unsupported agent). Read by the
/// check that automatic typing landed in the box before its Enter, a few
/// times per delivery, never per frame.
pub(crate) fn input_box(agent: Agent, ansi: &str) -> Option<PromptBox> {
    let lines: Vec<Vec<Cell>> = ansi.lines().map(parse_styled_line).collect();
    let (body, marker) = locate_box(agent, &lines)?;
    match classify(&lines[body], marker) {
        (prompt, true) => Some(prompt),
        (_, false) => None,
    }
}

/// The shortest run of a long paste's text (whitespace removed) that counts
/// as that paste when the box shows only part of it (it scrolled).
const PARTIAL_PASTE_CHARS: usize = 24;

/// `text` without any whitespace: a wrapped line, a joined box and the
/// typed text compare equal.
fn compact(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// Whether `typed` (a draft's text, [`PromptBox::Draft`]) shows `expected`,
/// the text herdr just typed: it contains all of it (whitespace ignored, so
/// a wrapped pointer line matches); for a paste (`paste`) also the agent's
/// placeholder for it (Claude Code `[Pasted text #N +K lines]`, Codex
/// `[Pasted Content N chars]`) or a long enough part of it.
pub(crate) fn draft_shows(typed: &str, expected: &str, paste: bool) -> bool {
    let (typed, expected) = (compact(typed), compact(expected));
    if expected.is_empty() {
        return false;
    }
    if typed.contains(&expected) {
        return true;
    }
    paste
        && (has_paste_placeholder(&typed)
            || (typed.chars().count() >= PARTIAL_PASTE_CHARS && expected.contains(&typed)))
}

/// Whether `typed` is exactly `expected` (whitespace ignored) or, for a
/// paste, exactly one paste placeholder: a box that still holds what herdr
/// typed and nothing else.
pub(crate) fn draft_is(typed: &str, expected: &str, paste: bool) -> bool {
    let (typed, expected) = (compact(typed), compact(expected));
    if !expected.is_empty() && typed == expected {
        return true;
    }
    paste
        && typed.starts_with("[Pasted")
        && typed.ends_with(']')
        && typed.matches(']').count() == 1
        && has_paste_placeholder(&typed)
}

/// Whether whitespace-free `typed` holds a paste placeholder:
/// `[Pastedtext#N]`, `[Pastedtext#N+Klines]` or `[PastedContentNchars]`.
fn has_paste_placeholder(typed: &str) -> bool {
    let digits = |text: &str| !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit());
    for prefix in ["[Pastedtext#", "[PastedContent"] {
        let mut rest = typed;
        while let Some(at) = rest.find(prefix) {
            let after = &rest[at + prefix.len()..];
            if let Some(end) = after.find(']') {
                let inner = &after[..end];
                let matched = if prefix == "[PastedContent" {
                    inner.strip_suffix("chars").is_some_and(digits)
                } else {
                    let split = inner
                        .find(|ch: char| !ch.is_ascii_digit())
                        .unwrap_or(inner.len());
                    let (number, more) = inner.split_at(split);
                    digits(number)
                        && (more.is_empty()
                            || more
                                .strip_prefix('+')
                                .and_then(|lines| lines.strip_suffix("lines"))
                                .is_some_and(digits))
                };
                if matched {
                    return true;
                }
            }
            rest = after;
        }
    }
    false
}

/// The guard for a pane's terminal runtime: `agent` is its known agent
/// (`None`: no agent, the recent-input check alone). The one typing guard:
/// agent messages, coordinator wake-ups (through `agent.prompt` with
/// `guard_user_typing`) and the agents model's close check all use it.
pub(crate) fn runtime_typing_block(
    agent: Option<Agent>,
    runtime: &crate::terminal::TerminalRuntime,
    now: Instant,
) -> Option<TypingBlock> {
    let last_user_input = runtime.last_user_input();
    match agent {
        Some(agent) => typing_block(agent, last_user_input, now, || {
            let rows = usize::from(runtime.current_size().0.max(1));
            runtime.recent_ansi_snapshot(rows).text
        }),
        None => last_user_input
            .is_some_and(|at| now.saturating_duration_since(at) < USER_INPUT_QUIET)
            .then_some(TypingBlock::RecentInput),
    }
}

fn draft_detection_supported(agent: Agent) -> bool {
    matches!(agent, Agent::Claude | Agent::Codex)
}

/// Whether the agent's input box (read from the bottom of the screen with
/// its SGR styles) holds non-placeholder text. `None` when the agent is not
/// supported or its input box is not on the screen. (Tests; the guard reads
/// [`composer`].)
#[cfg(test)]
pub(crate) fn unsent_draft(agent: Agent, ansi: &str) -> Option<bool> {
    prompt_box(agent, ansi).map(|prompt| matches!(prompt, PromptBox::Draft { .. }))
}

/// What the agent's input box holds, read from the screen with its styles.
/// The one classification behind the typing guard ([`composer`]) and the
/// agents_read annotation ([`annotate_prompt_box`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptBox {
    /// Only the prompt marker.
    Empty,
    /// Faint text only: a placeholder or suggestion the agent draws.
    Suggestion(String),
    /// Text in the default style: the user's own, not sent yet, with the
    /// faint text after it (an inline suggestion), if any.
    Draft {
        typed: String,
        suggestion: Option<String>,
    },
}

/// The input box of `agent` in `ansi`; `None` when the agent is not
/// supported or its input box is not on the screen. A blank body without
/// the prompt marker is not a box. (Tests; the guard reads [`composer`],
/// the agents_read mark [`annotate_prompt_box`].)
#[cfg(test)]
pub(crate) fn prompt_box(agent: Agent, ansi: &str) -> Option<PromptBox> {
    let lines: Vec<Vec<Cell>> = ansi.lines().map(parse_styled_line).collect();
    let (body, marker) = locate_box(agent, &lines)?;
    match classify(&lines[body], marker) {
        (PromptBox::Empty, false) => None,
        (prompt, _) => Some(prompt),
    }
}

/// Whether [`prompt_box`] and [`annotate_prompt_box`] know the agent's
/// input box.
pub(crate) fn prompt_box_supported(agent: Agent) -> bool {
    draft_detection_supported(agent)
}

/// The input box's body lines and prompt marker.
fn locate_box(agent: Agent, lines: &[Vec<Cell>]) -> Option<(Range<usize>, char)> {
    match agent {
        Agent::Claude => Some((claude_box_body(lines)?, '❯')),
        Agent::Codex => Some((codex_composer(lines)?, '›')),
        _ => None,
    }
}

/// A pane read's plain `text` with the agent's input box replaced by one
/// marked line, classified from `ansi` (the same screen read with its
/// styles): `❯ (suggestion shown by the agent, not typed by its user: "…")`
/// or `❯ (unsent draft typed by its user: "…")`. `None` (keep the text as
/// it is) when the box is empty or not recognised, or when `text` and
/// `ansi` do not show the same box (the screen changed between the two
/// reads). Runs only for an agents_read, never on render or detection
/// paths.
pub(crate) fn annotate_prompt_box(agent: Agent, text: &str, ansi: &str) -> Option<String> {
    let styled: Vec<Vec<Cell>> = ansi.lines().map(parse_styled_line).collect();
    let (styled_body, marker) = locate_box(agent, &styled)?;
    let note = match classify(&styled[styled_body.clone()], marker).0 {
        PromptBox::Empty => return None,
        PromptBox::Suggestion(shown) => {
            format!("(suggestion shown by the agent, not typed by its user: {shown:?})")
        }
        PromptBox::Draft {
            typed,
            suggestion: None,
        } => format!("(unsent draft typed by its user: {typed:?})"),
        PromptBox::Draft {
            typed,
            suggestion: Some(shown),
        } => format!(
            "(unsent draft typed by its user: {typed:?}; after it a suggestion shown by the agent, not typed by its user: {shown:?})"
        ),
    };
    let mut text_lines: Vec<&str> = text.split('\n').collect();
    let plain_lines: Vec<Vec<Cell>> = text_lines
        .iter()
        .map(|line| parse_styled_line(line))
        .collect();
    let (text_body, _) = locate_box(agent, &plain_lines)?;
    let same_box = styled_body.len() == text_body.len()
        && styled[styled_body]
            .iter()
            .zip(&plain_lines[text_body.clone()])
            .all(|(shown, read)| plain(shown).trim_end() == plain(read).trim_end());
    if !same_box {
        return None;
    }
    let first = plain(plain_lines.get(text_body.start)?);
    let from_marker = first.trim_start();
    if !from_marker.starts_with(marker) {
        return None;
    }
    let line = format!(
        "{}{marker} {note}",
        &first[..first.len() - from_marker.len()]
    );
    text_lines.splice(text_body, [line.as_str()]);
    Some(text_lines.join("\n"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    faint: bool,
}

fn plain(line: &[Cell]) -> String {
    line.iter().map(|cell| cell.ch).collect()
}

/// Claude Code: the lines between the last two horizontal rules.
fn claude_box_body(lines: &[Vec<Cell>]) -> Option<Range<usize>> {
    let mut rules = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| is_horizontal_rule(&plain(line)))
        .map(|(index, _)| index);
    let bottom = rules.next()?;
    let top = rules.next()?;
    Some(top + 1..bottom)
}

/// Codex: the last line starting with `›` and its continuation lines up to
/// the first blank line.
fn codex_composer(lines: &[Vec<Cell>]) -> Option<Range<usize>> {
    let start = lines
        .iter()
        .rposition(|line| line.first().is_some_and(|cell| cell.ch == '›'))?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.iter().all(|cell| cell.ch.is_whitespace()))
        .map_or(lines.len(), |relative| start + 1 + relative);
    Some(start..end)
}

/// A draft is any visible, non-faint character other than the leading
/// prompt marker; faint text alone is the agent's suggestion. Lines are
/// joined with a space. The flag: the body's first visible cell is the
/// prompt marker (a blank body, or one led by other text, is not a box the
/// typing guard may type into).
fn classify(body: &[Vec<Cell>], marker: char) -> (PromptBox, bool) {
    let mut first_seen = false;
    let mut marker_seen = false;
    let mut has_text = false;
    let (mut typed, mut shown) = (Vec::new(), Vec::new());
    for line in body {
        let (mut typed_line, mut shown_line) = (String::new(), String::new());
        for cell in line {
            if !first_seen && !cell.ch.is_whitespace() {
                first_seen = true;
                if cell.ch == marker {
                    marker_seen = true;
                    continue;
                }
            }
            if cell.faint {
                shown_line.push(cell.ch);
            } else {
                has_text |= !cell.ch.is_whitespace();
                typed_line.push(cell.ch);
            }
        }
        typed.push(typed_line);
        shown.push(shown_line);
    }
    let join = |parts: Vec<String>| {
        parts
            .iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (typed, shown) = (join(typed), join(shown));
    let prompt = match (has_text, shown.is_empty()) {
        (true, true) => PromptBox::Draft {
            typed,
            suggestion: None,
        },
        (true, false) => PromptBox::Draft {
            typed,
            suggestion: Some(shown),
        },
        (false, false) => PromptBox::Suggestion(shown),
        (false, true) => PromptBox::Empty,
    };
    (prompt, marker_seen)
}

/// Same shape as the detection manifests' rule check: a run of `─`, alone or
/// (with at least three) followed by a label.
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    let rule_chars = trimmed.chars().take_while(|&ch| ch == '─').count();
    if rule_chars == 0 {
        return false;
    }
    let rest = trimmed.chars().skip(rule_chars).collect::<String>();
    rest.trim().is_empty() || rule_chars >= 3
}

/// One screen line's characters with their faint (SGR 2) state. Escape
/// sequences other than SGR are dropped, as are control characters.
fn parse_styled_line(line: &str) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(line.len());
    let mut faint = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            if !ch.is_control() {
                cells.push(Cell { ch, faint });
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                let mut final_byte = None;
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        final_byte = Some(next);
                        break;
                    }
                    params.push(next);
                }
                if final_byte == Some('m') {
                    faint = apply_sgr(&params, faint);
                }
            }
            Some(']') => {
                // OSC: up to BEL or ST (ESC \).
                while let Some(next) = chars.next() {
                    if next == '\x07' {
                        break;
                    }
                    if next == '\x1b' {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    cells
}

fn apply_sgr(params: &str, mut faint: bool) -> bool {
    let mut parts = params.split(';');
    while let Some(part) = parts.next() {
        // Colon sub-parameters (`38:2:r:g:b`) stay inside one part.
        let code = part.split(':').next().unwrap_or("");
        match code {
            "" | "0" => faint = false,
            "2" => faint = true,
            "22" => faint = false,
            "38" | "48" | "58" if !part.contains(':') => match parts.next() {
                Some("5") => {
                    parts.next();
                }
                Some("2") => {
                    parts.next();
                    parts.next();
                    parts.next();
                }
                _ => {}
            },
            _ => {}
        }
    }
    faint
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "\x1b[0m\x1b[38;2;136;136;136m────────────────────\x1b[0m\r";
    const CLAUDE_FOOTER: &str = "  \x1b[0m\x1b[2m\x1b[38;2;153;153;153mHaiku 4.5\x1b[0m\r\n  \x1b[0m\x1b[38;2;153;153;153m⏸ manual mode on\x1b[0m";

    fn claude(body: &str) -> String {
        format!("\r\n\r\n{RULE}\n{body}\n{RULE}\n{CLAUDE_FOOTER}")
    }

    const CODEX_FOOTER: &str = "\r\n  \x1b[0m\x1b[38;2;246;226;183mGPT medium\x1b[0m · \x1b[0m\x1b[38;2;171;223;167m~/herdr\x1b[0m\r\n  \x1b[0m\x1b[1m? \x1b[0mfor shortcuts";

    fn codex(body: &str) -> String {
        format!("\r\n\x1b[0m\x1b[1m› \x1b[0m{body}\r\n{CODEX_FOOTER}")
    }

    #[test]
    fn claude_placeholder_and_empty_box_are_not_drafts() {
        let placeholder = claude("❯ \x1b[0m\x1b[2mTry \"write a test for model.rs\"\x1b[0m\r");
        assert_eq!(unsent_draft(Agent::Claude, &placeholder), Some(false));
        assert_eq!(unsent_draft(Agent::Claude, &claude("❯ \r")), Some(false));
    }

    #[test]
    fn claude_typed_text_is_a_draft_on_one_or_more_lines() {
        assert_eq!(
            unsent_draft(Agent::Claude, &claude("❯ hello draft text\r")),
            Some(true)
        );
        assert_eq!(
            unsent_draft(Agent::Claude, &claude("❯ \r\n  line two\r")),
            Some(true),
            "a continuation line counts"
        );
        // A draft that reads like the placeholder is still the user's text.
        assert_eq!(
            unsent_draft(Agent::Claude, &claude("❯ Try \"something\"\r")),
            Some(true)
        );
    }

    #[test]
    fn claude_text_outside_the_box_does_not_count() {
        let screen = format!(
            "previous answer text\r\n{}",
            claude("❯ \x1b[0m\x1b[2mTry \"x\"\x1b[0m\r")
        );
        assert_eq!(unsent_draft(Agent::Claude, &screen), Some(false));
    }

    #[test]
    fn no_recognised_box_is_unknown() {
        assert_eq!(unsent_draft(Agent::Claude, "just output\r\n❯ text"), None);
        assert_eq!(unsent_draft(Agent::Codex, "no composer here"), None);
        assert_eq!(unsent_draft(Agent::Pi, &claude("❯ text")), None);
    }

    #[test]
    fn codex_placeholder_is_empty_and_typed_text_is_a_draft() {
        let placeholder = codex("\x1b[2mAsk Codex to do anything\x1b[0m");
        assert_eq!(unsent_draft(Agent::Codex, &placeholder), Some(false));
        assert_eq!(unsent_draft(Agent::Codex, &codex("")), Some(false));
        assert_eq!(
            unsent_draft(Agent::Codex, &codex("codex draft here")),
            Some(true)
        );
        let multi =
            "\r\n\x1b[0m\x1b[1m› \x1b[0m\r\n  second codex line\r\n".to_string() + CODEX_FOOTER;
        assert_eq!(unsent_draft(Agent::Codex, &multi), Some(true));
    }

    #[test]
    fn codex_earlier_prompts_above_the_composer_do_not_count() {
        let screen = format!(
            "› an earlier prompt\r\n\r\n• answer\r\n{}",
            codex("\x1b[2mAsk Codex to do anything\x1b[0m")
        );
        assert_eq!(unsent_draft(Agent::Codex, &screen), Some(false));
    }

    #[test]
    fn sgr_parsing_tracks_faint_through_colors_and_resets() {
        let cells = parse_styled_line("a\x1b[2;38;2;1;2;3mb\x1b[22mc\x1b[2md\x1b[mE\x1b]0;t\x07f");
        let faint: Vec<(char, bool)> = cells.iter().map(|c| (c.ch, c.faint)).collect();
        assert_eq!(
            faint,
            vec![
                ('a', false),
                ('b', true),
                ('c', false),
                ('d', true),
                ('E', false),
                ('f', false)
            ]
        );
        // `38;5;2` is a palette color, not faint.
        assert!(!parse_styled_line("\x1b[38;5;2mx")[0].faint);
    }

    /// `claude(body)` as a plain-text read shows it.
    fn plain_text(ansi: &str) -> String {
        ansi.split('\n')
            .map(|line| plain(&parse_styled_line(line)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn prompt_box_classifies_suggestion_draft_and_empty() {
        let shown = claude("❯ \x1b[0m\x1b[2mok activate unitytest for the QA\x1b[0m\r");
        assert_eq!(
            prompt_box(Agent::Claude, &shown),
            Some(PromptBox::Suggestion(
                "ok activate unitytest for the QA".into()
            ))
        );
        assert_eq!(
            prompt_box(Agent::Claude, &claude("❯ ok go\r\n  on two lines\r")),
            Some(PromptBox::Draft {
                typed: "ok go on two lines".into(),
                suggestion: None
            })
        );
        assert_eq!(
            prompt_box(Agent::Claude, &claude("❯ ok act\x1b[2mivate it\x1b[0m\r")),
            Some(PromptBox::Draft {
                typed: "ok act".into(),
                suggestion: Some("ivate it".into())
            })
        );
        assert_eq!(
            prompt_box(Agent::Claude, &claude("❯ \r")),
            Some(PromptBox::Empty)
        );
        assert_eq!(
            prompt_box(
                Agent::Codex,
                &codex("\x1b[2mAsk Codex to do anything\x1b[0m")
            ),
            Some(PromptBox::Suggestion("Ask Codex to do anything".into()))
        );
        assert_eq!(prompt_box(Agent::Pi, &shown), None);
    }

    #[test]
    fn annotation_marks_a_faint_suggestion_as_not_typed_by_the_user() {
        let ansi = format!(
            "earlier output\r\n{}",
            claude("❯ \x1b[0m\x1b[2mok activate unitytest for the QA\x1b[0m\r")
        );
        let text = plain_text(&ansi);
        assert!(text.contains("❯ ok activate unitytest for the QA"));
        let annotated = annotate_prompt_box(Agent::Claude, &text, &ansi).expect("annotated");
        assert_eq!(
            annotated,
            text.replace(
                "❯ ok activate unitytest for the QA",
                "❯ (suggestion shown by the agent, not typed by its user: \"ok activate unitytest for the QA\")"
            )
        );
        assert!(annotated.starts_with("earlier output\n"));
    }

    #[test]
    fn annotation_marks_normal_weight_text_as_an_unsent_draft() {
        let ansi = claude("❯ ok activate it\r\n  please\r");
        let text = plain_text(&ansi);
        let annotated = annotate_prompt_box(Agent::Claude, &text, &ansi).expect("annotated");
        assert!(
            annotated.contains("\n❯ (unsent draft typed by its user: \"ok activate it please\")\n"),
            "{annotated}"
        );
        assert!(!annotated.contains("  please"), "the box is one line");
        let ansi = codex("codex draft here");
        let annotated =
            annotate_prompt_box(Agent::Codex, &plain_text(&ansi), &ansi).expect("annotated");
        assert!(
            annotated.contains("› (unsent draft typed by its user: \"codex draft here\")"),
            "{annotated}"
        );
    }

    /// The real screen readers (visible and recent, plain and styled) give
    /// the shapes the mark expects (synthetic faint input, no agent screen).
    #[tokio::test]
    async fn annotation_reads_the_terminal_visible_and_recent_screens() {
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(
            30,
            6,
            "done.\r\n────────────\r\n❯ \x1b[2mok activate it\x1b[0m\r\n────────────\r\n  footer"
                .as_bytes(),
        );
        let marked = "❯ (suggestion shown by the agent, not typed by its user: \"ok activate it\")";
        let visible = annotate_prompt_box(
            Agent::Claude,
            &runtime.visible_text(),
            &runtime.visible_ansi(),
        )
        .expect("visible screen marked");
        assert!(visible.contains(marked), "{visible}");
        let recent = annotate_prompt_box(
            Agent::Claude,
            &runtime.recent_text_snapshot(6).text,
            &runtime.recent_ansi_snapshot(6).text,
        )
        .expect("recent screen marked");
        assert!(recent.contains(marked), "{recent}");
    }

    #[test]
    fn no_annotation_for_an_empty_box_other_agents_or_a_changed_screen() {
        let empty = claude("❯ \r");
        assert_eq!(
            annotate_prompt_box(Agent::Claude, &plain_text(&empty), &empty),
            None
        );
        let shown = claude("❯ \x1b[2mTry \"x\"\x1b[0m\r");
        assert_eq!(
            annotate_prompt_box(Agent::Pi, &plain_text(&shown), &shown),
            None
        );
        // The plain read shows another box than the styled one.
        let typed = plain_text(&claude("❯ something else\r"));
        assert_eq!(annotate_prompt_box(Agent::Claude, &typed, &shown), None);
        assert_eq!(annotate_prompt_box(Agent::Claude, "no box", &shown), None);
    }

    #[test]
    fn recent_user_input_blocks_for_the_quiet_window_without_reading_the_screen() {
        let now = Instant::now();
        let read = || panic!("the screen is not read while the user types");
        assert_eq!(
            typing_block(Agent::Claude, Some(now), now, read),
            Some(TypingBlock::RecentInput)
        );
        let earlier = now.checked_sub(USER_INPUT_QUIET).expect("clock");
        assert_eq!(
            typing_block(Agent::Pi, Some(earlier), now, String::new),
            None,
            "the window has passed"
        );
    }

    /// Claude Code 2.1.299's screens, captured (`typing_guard/`).
    fn captured(name: &str) -> String {
        const SCREENS: &str = include_str!("typing_guard/claude_2_1_299_screens.ansi");
        let header = format!("### {name}\n");
        let start = SCREENS
            .find(&header)
            .unwrap_or_else(|| panic!("no captured screen {name}"))
            + header.len();
        let end = SCREENS[start..]
            .find("\n### ")
            .map_or(SCREENS.len(), |end| start + end);
        SCREENS[start..end].to_string()
    }

    #[test]
    fn captured_claude_screens_type_only_into_a_free_input_box() {
        for name in ["empty_box_auto_mode_statusline", "empty_box_manual_mode"] {
            assert_eq!(
                composer(Agent::Claude, &captured(name)),
                Composer::Free,
                "{name}"
            );
        }
        // The `/` picker draws over the transcript; its box holds the `/`.
        for name in ["draft", "slash_picker"] {
            assert_eq!(
                composer(Agent::Claude, &captured(name)),
                Composer::Draft,
                "{name}"
            );
        }
        // AskUserQuestion (one question, three with a multiSelect, its
        // Submit page, after a resize), a Bash permission prompt and the
        // plan approval: no input box led by its marker. The Submit page,
        // the permission prompt and the plan approval draw one rule only
        // (the old guard found no box there and let typing through); the
        // plan approval also has an older rule above it on the screen.
        for name in [
            "ask_user_question",
            "ask_user_question_resized",
            "ask_user_question_three",
            "ask_user_question_multiselect",
            "ask_user_question_submit",
            "bash_permission",
            "plan_exit",
        ] {
            assert_eq!(
                composer(Agent::Claude, &captured(name)),
                Composer::Absent,
                "{name}"
            );
        }
    }

    #[test]
    fn a_dialog_holds_typing_like_a_draft_with_its_own_reason() {
        let now = Instant::now();
        let block = typing_block(Agent::Claude, None, now, || captured("bash_permission"));
        assert_eq!(block, Some(TypingBlock::InputBoxAbsent));
        let block = block.expect("held");
        assert_eq!(block.error_code(), "input_box_absent");
        assert!(!block.is_user_typing());
        assert_eq!(
            block.reason(),
            "its input box is not on screen (a dialog or a menu?)"
        );
        assert_eq!(
            typing_block(Agent::Claude, None, now, || captured("draft"))
                .map(TypingBlock::error_code),
            Some("user_typing")
        );
        assert_eq!(
            typing_block(Agent::Claude, None, now, || {
                captured("empty_box_auto_mode_statusline")
            }),
            None
        );
        // No box at all (an unknown screen) fails closed too.
        assert_eq!(
            typing_block(Agent::Claude, None, now, || "plain output".to_string()),
            Some(TypingBlock::InputBoxAbsent)
        );
        assert_eq!(
            typing_block(Agent::Codex, None, now, String::new),
            Some(TypingBlock::InputBoxAbsent)
        );
        // Other agents keep the recent-input check alone.
        assert_eq!(
            typing_block(Agent::Pi, None, now, || panic!("not read")),
            None
        );
    }

    #[test]
    fn the_test_input_box_is_free_for_claude_and_codex() {
        let screen = String::from_utf8_lossy(TEST_FREE_INPUT_BOX);
        assert_eq!(composer(Agent::Claude, &screen), Composer::Free);
        assert_eq!(composer(Agent::Codex, &screen), Composer::Free);
    }

    #[test]
    fn a_blank_body_without_the_marker_is_not_a_box() {
        // Two rules with nothing but blanks between them (a dialog's frame,
        // the last two rules of the screen) used to read as an empty box.
        let blank = claude("   \r");
        assert_eq!(prompt_box(Agent::Claude, &blank), None);
        assert_eq!(composer(Agent::Claude, &blank), Composer::Absent);
        // Faint text or other text first, without the marker: not a box
        // typing may go into.
        assert_eq!(
            composer(Agent::Claude, &claude("\x1b[2m  Esc to cancel\x1b[0m\r")),
            Composer::Absent
        );
        assert_eq!(
            composer(Agent::Claude, &claude(" ☐ Drink\r\n❯ 1. Tea\r")),
            Composer::Absent
        );
        assert_eq!(composer(Agent::Claude, &claude("❯ \r")), Composer::Free);
        assert_eq!(
            composer(
                Agent::Codex,
                &codex("\x1b[2mAsk Codex to do anything\x1b[0m")
            ),
            Composer::Free
        );
    }

    #[test]
    fn a_dialog_under_an_older_box_reads_absent_not_empty() {
        // The last two rules: the old box's lower rule (scrolled up) and
        // the dialog's top rule; the transcript between them is not a box.
        let screen = format!(
            "{RULE}\n❯ earlier prompt\r\n{RULE}\n\r\n⏺ answer text\r\n{RULE}\n Do you want to proceed?\r\n ❯ 1. Yes\r\n   2. No\r"
        );
        assert_eq!(composer(Agent::Claude, &screen), Composer::Absent);
        let screen = format!("{RULE}\n\r\n\r\n{RULE}\n Do you want to proceed?\r\n ❯ 1. Yes\r");
        assert_eq!(composer(Agent::Claude, &screen), Composer::Absent);
    }

    #[test]
    fn a_draft_blocks_supported_agents_only() {
        let now = Instant::now();
        let draft = || claude("❯ draft\r");
        assert_eq!(
            typing_block(Agent::Claude, None, now, draft),
            Some(TypingBlock::UnsentDraft)
        );
        assert_eq!(
            typing_block(Agent::Claude, None, now, || claude("❯ \r")),
            None
        );
        assert_eq!(
            typing_block(Agent::Pi, None, now, || panic!("not read")),
            None
        );
    }
}
