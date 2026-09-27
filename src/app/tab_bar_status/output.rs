//! Fork: `lines` and `ansi` on `ui.tab_bar_right` command entries.
//!
//! With the defaults (`lines = 1`, `ansi = false`) every function here hands
//! off to upstream's reader and sanitizer unchanged, so the segment text is
//! byte-for-byte upstream's. `lines > 1` keeps the last `lines` non-empty
//! output lines joined with `\n`; `ansi = true` keeps SGR sequences
//! (`ESC [ <digits and ;> m`) and still strips every other control sequence
//! and control character. The segment stays a plain string on the wire; the
//! client splits it back into rows and parses the SGR.

use std::collections::VecDeque;

use tokio::io::AsyncReadExt;

use super::{
    command_output_text, is_unicode_format_control, read_last_output_line, MAX_COMMAND_LINE_BYTES,
    MAX_STATUS_TEXT_CHARS,
};

/// How a command entry's output becomes its segment text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct StatusOutputFormat {
    lines: u8,
    ansi: bool,
}

impl StatusOutputFormat {
    /// Upstream's behaviour: the sanitized last line, escapes stripped.
    pub(in crate::app) const UPSTREAM: Self = Self {
        lines: 1,
        ansi: false,
    };

    pub(in crate::app) fn new(lines: u8, ansi: bool) -> Self {
        Self {
            lines: crate::config::effective_tab_bar_command_lines(lines),
            ansi,
        }
    }
}

/// Reads the output lines `status_output_text` needs: upstream's last line
/// for `lines = 1`, else the last `lines` lines that are visibly non-empty.
pub(super) async fn read_status_output_lines(
    mut stdout: tokio::process::ChildStdout,
    format: StatusOutputFormat,
) -> std::io::Result<Vec<Vec<u8>>> {
    if format.lines <= 1 {
        return read_last_output_line(stdout).await.map(|line| vec![line]);
    }

    let keep = usize::from(format.lines);
    let mut kept = VecDeque::with_capacity(keep);
    let mut current_line = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stdout.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        for &byte in &buffer[..count] {
            if byte == b'\n' {
                keep_visible_line(&mut kept, keep, std::mem::take(&mut current_line), format);
            } else if current_line.len() < MAX_COMMAND_LINE_BYTES {
                current_line.push(byte);
            }
        }
    }
    keep_visible_line(&mut kept, keep, current_line, format);
    Ok(kept.into())
}

fn keep_visible_line(
    kept: &mut VecDeque<Vec<u8>>,
    keep: usize,
    line: Vec<u8>,
    format: StatusOutputFormat,
) {
    if sanitize_output_line(&line, format.ansi).is_none() {
        return;
    }
    if kept.len() == keep {
        kept.pop_front();
    }
    kept.push_back(line);
}

/// The segment text for a successful run.
pub(super) fn status_output_text(lines: &[Vec<u8>], format: StatusOutputFormat) -> Option<String> {
    if format == StatusOutputFormat::UPSTREAM {
        return command_output_text(lines.last().map(Vec::as_slice).unwrap_or_default());
    }
    let kept = lines
        .iter()
        .filter_map(|line| sanitize_output_line(line, format.ansi))
        .collect::<Vec<_>>();
    let start = kept.len().saturating_sub(usize::from(format.lines));
    let text = kept[start..].join("\n");
    (!text.is_empty()).then_some(text)
}

/// One output line (no `\n`) as segment text, `None` when nothing visible
/// remains.
fn sanitize_output_line(line: &[u8], ansi: bool) -> Option<String> {
    if !ansi {
        return command_output_text(line);
    }
    let line = String::from_utf8_lossy(line);
    let line = strip_control_sequences_keeping_sgr(line.as_bytes());
    let line = String::from_utf8_lossy(&line);
    sanitize_styled_text(line.lines().next_back().unwrap_or_default())
}

#[derive(Clone, Copy)]
enum ControlSequenceState {
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    StString,
}

/// Upstream's `strip_terminal_control_sequences`, except that a CSI whose
/// final byte is `m` and whose parameters are only digits and `;` is kept.
fn strip_control_sequences_keeping_sgr(value: &[u8]) -> Vec<u8> {
    use ControlSequenceState::*;

    let mut output = Vec::with_capacity(value.len());
    let mut params = Vec::new();
    let mut state = Text;
    for &byte in value {
        state = match (state, byte) {
            (Text, b'\x1b') => Escape,
            (Text, _) => {
                output.push(byte);
                Text
            }
            (Escape, b'[') => {
                params.clear();
                Csi
            }
            (Escape, b']') => Osc,
            (Escape, b'P' | b'X' | b'^' | b'_') => StString,
            (Escape, 0x20..=0x2f) => EscapeIntermediate,
            (Escape, 0x30..=0x7e) => Text,
            (Escape, b'\x1b') => Escape,
            (Escape, b'\x18' | b'\x1a') => Text,
            (Escape, byte) if byte.is_ascii_control() => Escape,
            (Escape, _) => {
                output.push(byte);
                Text
            }
            (EscapeIntermediate, 0x20..=0x2f) => EscapeIntermediate,
            (EscapeIntermediate, 0x30..=0x7e) => Text,
            (EscapeIntermediate, b'\x1b') => Escape,
            (EscapeIntermediate, b'\x18' | b'\x1a') => Text,
            (EscapeIntermediate, byte) if byte.is_ascii_control() => EscapeIntermediate,
            (EscapeIntermediate, _) => {
                output.push(byte);
                Text
            }
            (Csi, 0x20..=0x3f) => {
                params.push(byte);
                Csi
            }
            (Csi, b'm') => {
                if params
                    .iter()
                    .all(|param| param.is_ascii_digit() || *param == b';')
                {
                    output.extend_from_slice(b"\x1b[");
                    output.extend_from_slice(&params);
                    output.push(b'm');
                }
                Text
            }
            (Csi, 0x40..=0x7e) => Text,
            (Csi, b'\x1b') => Escape,
            (Csi, b'\x18' | b'\x1a') => Text,
            (Csi, byte) if byte.is_ascii_control() => Csi,
            (Csi, _) => {
                output.push(byte);
                Text
            }
            (Osc, b'\x07') => Text,
            (Osc, b'\x1b') => Escape,
            (Osc, b'\x18' | b'\x1a') => Text,
            (Osc, _) => Osc,
            (StString, b'\x1b') => Escape,
            (StString, b'\x18' | b'\x1a') => Text,
            (StString, _) => StString,
        };
    }
    output
}

/// Upstream's `sanitize_status_text` for text holding kept SGR sequences:
/// drops control and format characters outside them, counts only visible
/// characters against the cap, and is `None` without any visible character.
fn sanitize_styled_text(value: &str) -> Option<String> {
    let mut output = String::with_capacity(value.len());
    let mut visible = 0;
    let mut chars = value.trim().chars();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            // Only kept SGR sequences reach here: `ESC [ <digits ;> m`.
            output.push(character);
            for sequence in chars.by_ref() {
                output.push(sequence);
                if sequence == 'm' {
                    break;
                }
            }
            continue;
        }
        if character.is_control() || is_unicode_format_control(character) {
            continue;
        }
        if visible == MAX_STATUS_TEXT_CHARS {
            break;
        }
        output.push(character);
        visible += 1;
    }
    (visible > 0).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[&[u8]], format: StatusOutputFormat) -> Option<String> {
        let lines = lines.iter().map(|line| line.to_vec()).collect::<Vec<_>>();
        status_output_text(&lines, format)
    }

    #[test]
    fn defaults_are_upstreams_sanitized_last_line() {
        let format = StatusOutputFormat::new(1, false);
        assert_eq!(format, StatusOutputFormat::UPSTREAM);
        assert_eq!(
            text(&[b"old\n win\x1b[31mter\r"], format),
            command_output_text(b"old\n win\x1b[31mter\r")
        );
        assert_eq!(
            text(&[b"\x1b[32mHELLO\x1b[0m"], format),
            Some("HELLO".into())
        );
        assert_eq!(text(&[], format), None);
    }

    #[test]
    fn ansi_keeps_sgr_and_strips_every_other_sequence() {
        let format = StatusOutputFormat::new(1, true);
        assert_eq!(
            text(
                &[b"\x1b[1;38;2;1;2;3mok\x1b[0m \x1b[38;5;208mwarn\x1b[m"],
                format
            ),
            Some("\x1b[1;38;2;1;2;3mok\x1b[0m \x1b[38;5;208mwarn\x1b[m".into())
        );
        // Cursor moves, erase, private modes, colon SGR, OSC links, DCS and
        // bare controls all go.
        assert_eq!(
            text(
                &[b"\x1b[2J\x1b[H\x1b[?25l\x1b[3A\x1b[38:5:1ma\x1b]8;;https://x\x1b\\b\x1b]8;;\x07\x1bPdcs\x1b\\c\x07\x08\td\x1b7"],
                format,
            ),
            Some("abcd".into())
        );
        // A line with only SGR is empty.
        assert_eq!(text(&[b"\x1b[31m\x1b[0m"], format), None);
    }

    #[test]
    fn ansi_caps_visible_characters_not_escapes() {
        let styled = format!("\x1b[38;2;1;2;3m{}\x1b[0m", "x".repeat(100));
        let kept =
            text(&[styled.as_bytes()], StatusOutputFormat::new(1, true)).expect("visible text");
        assert_eq!(kept, format!("\x1b[38;2;1;2;3m{}", "x".repeat(80)));
    }

    #[test]
    fn lines_keeps_the_last_non_empty_lines_joined_with_newlines() {
        let format = StatusOutputFormat::new(3, false);
        assert_eq!(
            text(
                &[b"one", b"two", b"\x1b[31m\x1b[0m", b"three\r", b"four"],
                format
            ),
            Some("two\nthree\nfour".into())
        );
        assert_eq!(
            text(&[b"a\x1b[31mb"], StatusOutputFormat::new(4, true)),
            Some("a\x1b[31mb".into())
        );
        // Clamped to four.
        assert_eq!(
            text(
                &[b"1", b"2", b"3", b"4", b"5"],
                StatusOutputFormat::new(9, false)
            ),
            Some("2\n3\n4\n5".into())
        );
        assert_eq!(text(&[b"   ", b""], format), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn multi_line_ansi_command_reports_its_last_lines() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        super::super::spawn_status_command_with_format(
            event_tx,
            1,
            0,
            "printf 'zero\\n\\033[31mone\\033[0m\\n\\n\\033[2Ktwo\\nthree\\n\\n'".into(),
            std::time::Duration::from_secs(2),
            Vec::new(),
            None,
            StatusOutputFormat::new(3, true),
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), event_rx.recv())
            .await
            .expect("status command timed out")
            .expect("status command event channel closed");
        assert!(
            matches!(
                event,
                crate::events::AppEvent::TabBarCommandFinished {
                    result: Ok(Some(ref output)),
                    ..
                } if output == "\x1b[31mone\x1b[0m\ntwo\nthree"
            ),
            "{event:?}"
        );
    }
}
