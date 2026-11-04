//! Fork (scroll perf): applies a program's synchronized-output frame to the
//! terminal in one piece.
//!
//! A program that redraws inside `CSI ? 2026 h` … `CSI ? 2026 l` (agent UIs,
//! pagers) reaches the PTY reader in small chunks (about 1 KiB on macOS). Fed
//! chunk by chunk, the pane's terminal sits in synchronized mode for most of a
//! fast redraw stream, every render that lands there is deferred, and a
//! wheel-driven TUI freezes for 50-500 ms at a time before it catches up.
//!
//! [`SyncFrameHold`] holds the bytes of an open synchronized block until its
//! closing sequence arrives, so the parser takes the whole frame at once and a
//! render never finds the pane in the middle of one. Holding changes nothing
//! visible (herdr does not render a pane in synchronized mode anyway); it only
//! delays parsing of the open frame. The hold is bounded: after
//! [`SYNC_FRAME_HOLD_TIMEOUT`] or [`SYNC_FRAME_HOLD_MAX_BYTES`] the held bytes
//! are released and the rest of that block passes through as before, so a
//! program that leaves synchronized mode on (or answers a query mid-frame)
//! waits at most one timeout. The scan matches only the exact 8-byte forms
//! `ESC [ ? 2026 h` / `ESC [ ? 2026 l`; anything else (combined modes, C1 CSI)
//! is passed through unheld, as before. Time is injected, so the rules are pure.

use std::time::{Duration, Instant};

/// How long an open synchronized block's bytes may be held.
pub(crate) const SYNC_FRAME_HOLD_TIMEOUT: Duration = Duration::from_millis(10);
/// The most bytes held for one block before they are released.
pub(crate) const SYNC_FRAME_HOLD_MAX_BYTES: usize = 1 << 20;

/// `ESC [ ? 2026`, followed by `h` (open) or `l` (close).
const SYNC_PREFIX: &[u8] = b"\x1b[?2026";

/// What the reader parses after [`SyncFrameHold::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncRelease {
    /// Parse the whole chunk now (nothing is held).
    Chunk,
    /// Parse `chunk[..n]` now; the rest is held (`n` may be 0).
    Prefix(usize),
    /// Parse [`SyncFrameHold::take_released`]`(n)` now: held bytes up to `n`.
    Held(usize),
    /// Parse nothing yet; the chunk joined the held frame.
    None,
}

#[derive(Debug)]
pub(crate) struct SyncFrameHold {
    held: Vec<u8>,
    /// How much of `SYNC_PREFIX` the stream's tail matches.
    matched: usize,
    /// The stream is inside a synchronized block (per this scan).
    open: bool,
    /// The current block was released (timeout, cap or flush); it passes
    /// through until it closes.
    released: bool,
    deadline: Option<Instant>,
}

impl Default for SyncFrameHold {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncFrameHold {
    pub(crate) fn new() -> Self {
        Self {
            held: Vec::new(),
            matched: 0,
            open: false,
            released: false,
            deadline: None,
        }
    }

    /// Whether bytes are held.
    #[cfg(test)]
    pub(crate) fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// When the held bytes must be released, if any are held.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Feeds one chunk read from the PTY.
    pub(crate) fn push(&mut self, chunk: &[u8], now: Instant) -> SyncRelease {
        let was_holding = !self.held.is_empty();
        // Start (in chunk coordinates, clamped to the chunk) of a block opened in
        // this chunk and still open at its end.
        let open_start = self.scan(chunk);
        if !was_holding {
            return match open_start {
                Some(start) if self.open && !self.released => {
                    self.held.extend_from_slice(&chunk[start..]);
                    self.deadline = Some(now + SYNC_FRAME_HOLD_TIMEOUT);
                    SyncRelease::Prefix(start)
                }
                _ => SyncRelease::Chunk,
            };
        }
        let base = self.held.len();
        self.held.extend_from_slice(chunk);
        if !self.open {
            self.deadline = None;
            return SyncRelease::Held(self.held.len());
        }
        if let Some(start) = open_start {
            // The held frame closed in this chunk and a new one opened.
            self.deadline = Some(now + SYNC_FRAME_HOLD_TIMEOUT);
            return SyncRelease::Held(base + start);
        }
        if self.held.len() > SYNC_FRAME_HOLD_MAX_BYTES {
            return self.release_open_block();
        }
        SyncRelease::None
    }

    /// Releases the held bytes once their deadline has passed.
    pub(crate) fn expire(&mut self, now: Instant) -> SyncRelease {
        match self.deadline {
            Some(deadline) if now >= deadline => self.release_open_block(),
            _ => SyncRelease::None,
        }
    }

    /// Releases everything held (handoff, EOF); an open block passes through.
    pub(crate) fn flush(&mut self) -> SyncRelease {
        if self.held.is_empty() {
            self.released |= self.open;
            return SyncRelease::None;
        }
        self.release_open_block()
    }

    /// Takes the first `n` held bytes after a [`SyncRelease::Held`].
    pub(crate) fn take_released(&mut self, n: usize) -> Vec<u8> {
        let n = n.min(self.held.len());
        let rest = self.held.split_off(n);
        std::mem::replace(&mut self.held, rest)
    }

    fn release_open_block(&mut self) -> SyncRelease {
        self.released = self.open;
        self.deadline = None;
        if self.held.is_empty() {
            SyncRelease::None
        } else {
            SyncRelease::Held(self.held.len())
        }
    }

    /// Advances the open/close state over `chunk`. Returns where a block that
    /// opened in this chunk and is still open begins (0 when its `ESC` came in
    /// an earlier chunk).
    fn scan(&mut self, chunk: &[u8]) -> Option<usize> {
        let mut open_start = None;
        let mut i = 0;
        while i < chunk.len() {
            if self.matched == 0 {
                let Some(offset) = chunk[i..].iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                i += offset + 1;
                self.matched = 1;
                continue;
            }
            let byte = chunk[i];
            if self.matched == SYNC_PREFIX.len() {
                match byte {
                    b'h' if !self.open => {
                        self.open = true;
                        open_start = Some((i + 1).saturating_sub(SYNC_PREFIX.len() + 1));
                    }
                    b'l' => {
                        self.open = false;
                        self.released = false;
                        open_start = None;
                    }
                    _ => {}
                }
                self.matched = usize::from(byte == 0x1b);
            } else if byte == SYNC_PREFIX[self.matched] {
                self.matched += 1;
            } else {
                self.matched = usize::from(byte == 0x1b);
            }
            i += 1;
        }
        open_start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPEN: &[u8] = b"\x1b[?2026h";
    const CLOSE: &[u8] = b"\x1b[?2026l";

    /// Feeds chunks and collects what the parser receives, in order.
    struct Reader {
        hold: SyncFrameHold,
        parsed: Vec<Vec<u8>>,
    }

    impl Reader {
        fn new() -> Self {
            Self {
                hold: SyncFrameHold::new(),
                parsed: Vec::new(),
            }
        }

        fn apply(&mut self, chunk: &[u8], release: SyncRelease) {
            match release {
                SyncRelease::Chunk => self.parsed.push(chunk.to_vec()),
                SyncRelease::Prefix(n) if n > 0 => self.parsed.push(chunk[..n].to_vec()),
                SyncRelease::Prefix(_) | SyncRelease::None => {}
                SyncRelease::Held(n) => {
                    let bytes = self.hold.take_released(n);
                    self.parsed.push(bytes);
                }
            }
        }

        fn push(&mut self, chunk: &[u8], now: Instant) {
            let release = self.hold.push(chunk, now);
            self.apply(chunk, release);
        }

        fn expire(&mut self, now: Instant) {
            let release = self.hold.expire(now);
            self.apply(&[], release);
        }

        fn all(&self) -> Vec<u8> {
            self.parsed.concat()
        }
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        [OPEN, body, CLOSE].concat()
    }

    #[test]
    fn plain_output_passes_through_unheld() {
        let now = Instant::now();
        let mut reader = Reader::new();
        reader.push(b"hello \x1b[31mworld\x1b[0m\r\n", now);
        reader.push(b"\x1b[?2026$p\x1b[?25h", now);
        assert_eq!(reader.parsed.len(), 2);
        assert!(!reader.hold.is_holding());
        assert_eq!(reader.hold.deadline(), None);
    }

    #[test]
    fn a_frame_split_across_chunks_is_parsed_in_one_piece() {
        let now = Instant::now();
        let mut reader = Reader::new();
        let bytes = [
            b"before".as_slice(),
            &frame(b"row one\r\nrow two"),
            b"after",
        ]
        .concat();
        // Split inside the opening, the body and the closing sequence.
        for cut in bytes.chunks(3) {
            reader.push(cut, now);
        }
        assert_eq!(reader.all(), bytes);
        // No parse call ends with the stream inside the frame: the mode turns
        // on and off within one call.
        let mut scan = SyncFrameHold::new();
        for part in &reader.parsed {
            scan.scan(part);
            assert!(!scan.open, "a parse call left the frame open: {part:?}");
        }
        assert!(!reader.hold.is_holding());
    }

    #[test]
    fn a_closed_frame_followed_by_a_new_one_releases_only_the_closed_frame() {
        let now = Instant::now();
        let mut reader = Reader::new();
        reader.push(b"x\x1b[?2026hframe one", now);
        assert_eq!(reader.parsed, vec![b"x".to_vec()]);
        assert!(reader.hold.is_holding());
        reader.push(b" end\x1b[?2026l\x1b[?2026hframe two", now);
        assert_eq!(reader.parsed[1], frame(b"frame one end"));
        assert!(reader.hold.is_holding());
        reader.push(b"\x1b[?2026l", now);
        assert_eq!(reader.parsed[2], frame(b"frame two"));
        assert!(!reader.hold.is_holding());
        assert_eq!(reader.hold.deadline(), None);
    }

    #[test]
    fn several_whole_frames_in_one_chunk_pass_as_the_chunk() {
        let now = Instant::now();
        let mut reader = Reader::new();
        let bytes = [frame(b"a"), frame(b"b"), b"tail".to_vec()].concat();
        let release = reader.hold.push(&bytes, now);
        assert_eq!(release, SyncRelease::Chunk);
    }

    #[test]
    fn a_held_frame_is_released_at_its_deadline_and_the_block_then_passes_through() {
        let start = Instant::now();
        let mut reader = Reader::new();
        reader.push(b"\x1b[?2026hstuck", start);
        assert_eq!(
            reader.hold.deadline(),
            Some(start + SYNC_FRAME_HOLD_TIMEOUT)
        );
        reader.expire(start + SYNC_FRAME_HOLD_TIMEOUT - Duration::from_millis(1));
        assert!(reader.hold.is_holding());
        reader.expire(start + SYNC_FRAME_HOLD_TIMEOUT);
        assert_eq!(reader.all(), b"\x1b[?2026hstuck");
        assert!(!reader.hold.is_holding());
        // The rest of that block is not held again.
        let later = start + SYNC_FRAME_HOLD_TIMEOUT * 2;
        assert_eq!(reader.hold.push(b" more", later), SyncRelease::Chunk);
        assert_eq!(reader.hold.push(CLOSE, later), SyncRelease::Chunk);
        // The next block is held again.
        assert_eq!(reader.hold.push(OPEN, later), SyncRelease::Prefix(0));
        assert!(reader.hold.is_holding());
    }

    #[test]
    fn an_oversized_frame_is_released_and_passes_through() {
        let now = Instant::now();
        let mut reader = Reader::new();
        reader.push(OPEN, now);
        let body = vec![b'x'; 64 * 1024];
        for _ in 0..(SYNC_FRAME_HOLD_MAX_BYTES / body.len() - 1) {
            reader.push(&body, now);
        }
        assert!(reader.hold.is_holding());
        reader.push(&body, now);
        assert!(!reader.hold.is_holding());
        assert_eq!(reader.hold.push(&body, now), SyncRelease::Chunk);
        assert_eq!(reader.all().len(), OPEN.len() + body.len() * 16);
    }

    #[test]
    fn flush_releases_held_bytes_and_lets_the_block_pass_through() {
        let now = Instant::now();
        let mut reader = Reader::new();
        reader.push(b"\x1b[?2026hpartial", now);
        let release = reader.hold.flush();
        reader.apply(&[], release);
        assert_eq!(reader.all(), b"\x1b[?2026hpartial");
        assert_eq!(reader.hold.push(b"rest", now), SyncRelease::Chunk);
        assert_eq!(reader.hold.flush(), SyncRelease::None);
    }

    #[test]
    fn an_opening_split_after_its_escape_holds_from_the_next_chunk() {
        let now = Instant::now();
        let mut reader = Reader::new();
        reader.push(b"text\x1b[?20", now);
        assert_eq!(reader.hold.push(b"26hbody", now), SyncRelease::Prefix(0));
        assert!(reader.hold.is_holding());
    }

    #[test]
    fn a_repeated_opening_inside_a_block_keeps_one_frame() {
        let now = Instant::now();
        let mut reader = Reader::new();
        reader.push(b"\x1b[?2026ha\x1b[?2026hb", now);
        assert!(reader.parsed.is_empty());
        reader.push(CLOSE, now);
        assert_eq!(reader.all(), b"\x1b[?2026ha\x1b[?2026hb\x1b[?2026l");
    }
}
