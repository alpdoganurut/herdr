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

use std::time::{Duration, Instant};

use crate::detect::Agent;

/// How long after the user's last keystroke in a pane automatic typing waits.
pub(crate) const USER_INPUT_QUIET: Duration = Duration::from_secs(10);

/// Why automatic typing is held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypingBlock {
    /// The user typed into the pane within [`USER_INPUT_QUIET`].
    RecentInput,
    /// The agent's input box holds text the user has not sent.
    UnsentDraft,
}

impl TypingBlock {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            TypingBlock::RecentInput => "the user is typing in it",
            TypingBlock::UnsentDraft => "the user has an unsent draft in its input box",
        }
    }
}

/// The guard's decision. `bottom_ansi` is only read when the recent-input
/// check passes and the agent's input box is recognised.
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
    (unsent_draft(agent, &bottom_ansi()) == Some(true)).then_some(TypingBlock::UnsentDraft)
}

fn draft_detection_supported(agent: Agent) -> bool {
    matches!(agent, Agent::Claude | Agent::Codex)
}

/// Whether the agent's input box (read from the bottom of the screen with
/// its SGR styles) holds non-placeholder text. `None` when the agent is not
/// supported or its input box is not on the screen.
pub(crate) fn unsent_draft(agent: Agent, ansi: &str) -> Option<bool> {
    let lines: Vec<Vec<Cell>> = ansi.lines().map(parse_styled_line).collect();
    let (body, marker) = match agent {
        Agent::Claude => (claude_box_body(&lines)?, '❯'),
        Agent::Codex => (codex_composer(&lines)?, '›'),
        _ => return None,
    };
    Some(body_has_text(body, marker))
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
fn claude_box_body(lines: &[Vec<Cell>]) -> Option<&[Vec<Cell>]> {
    let mut rules = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| is_horizontal_rule(&plain(line)))
        .map(|(index, _)| index);
    let bottom = rules.next()?;
    let top = rules.next()?;
    Some(&lines[top + 1..bottom])
}

/// Codex: the last line starting with `›` and its continuation lines up to
/// the first blank line.
fn codex_composer(lines: &[Vec<Cell>]) -> Option<&[Vec<Cell>]> {
    let start = lines
        .iter()
        .rposition(|line| line.first().is_some_and(|cell| cell.ch == '›'))?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.iter().all(|cell| cell.ch.is_whitespace()))
        .map_or(lines.len(), |relative| start + 1 + relative);
    Some(&lines[start..end])
}

/// Any visible, non-faint character other than the leading prompt marker.
fn body_has_text(body: &[Vec<Cell>], marker: char) -> bool {
    let mut marker_seen = false;
    for cell in body.iter().flatten() {
        if cell.ch.is_whitespace() {
            continue;
        }
        if !marker_seen {
            marker_seen = true;
            if cell.ch == marker {
                continue;
            }
        }
        if !cell.faint {
            return true;
        }
    }
    false
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
