//! The non-user-turn marker (`turn.json`): written before herdr+ itself
//! prompts the coordinator (a wake-up or an agent message), so the MCP write
//! tools can tell that turn was not started by the user. The watcher steps
//! the marker with [`advance`] each tick and clears it when the turn is over.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{turn_path, write_atomically};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// `wake` or `message`.
    pub source: String,
    pub id: String,
    pub started_unix: u64,
    pub coordinator_pane: String,
    #[serde(default)]
    pub seen_working: bool,
}

pub const TURN_HARD_EXPIRY_S: u64 = 600;
pub const TURN_NEVER_WORKED_S: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStep {
    Keep,
    MarkWorking,
    Clear,
}

/// Write the marker atomically.
pub fn write(dir: &Path, turn: &Turn) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(turn).map_err(io::Error::other)?;
    write_atomically(&turn_path(dir), &json)
}

/// The marker, unless it is absent, unparseable or past the hard expiry.
pub fn read_live(dir: &Path, now: u64) -> Option<Turn> {
    let bytes = std::fs::read(turn_path(dir)).ok()?;
    let turn: Turn = serde_json::from_slice(&bytes).ok()?;
    (now.saturating_sub(turn.started_unix) < TURN_HARD_EXPIRY_S).then_some(turn)
}

pub fn clear(dir: &Path) {
    if let Err(err) = std::fs::remove_file(turn_path(dir)) {
        if err.kind() != io::ErrorKind::NotFound {
            tracing::warn!("herdr+ cannot clear the turn marker: {err}");
        }
    }
}

/// Pure: given the live turn and the coordinator's current status, the next
/// marker state. In order: past the hard expiry the turn is over whatever the
/// status (a coordinator stuck working must not lock the user out forever);
/// `working` is recorded once (`Keep` when already seen, so the marker is not
/// rewritten every tick), and so is `blocked`: herdr+ only prompts an idle
/// coordinator, so blocked means the turn started and hit a prompt (a poll
/// may miss the working in between); after working, `idle`/`done` ends the turn; a turn
/// that never showed `working` within [`TURN_NEVER_WORKED_S`] was swallowed
/// or instant and ends too. Anything else (`blocked`, `unknown`, ...) keeps it.
pub fn advance(turn: &Turn, status: &str, now: u64) -> TurnStep {
    let age = now.saturating_sub(turn.started_unix);
    if age >= TURN_HARD_EXPIRY_S {
        return TurnStep::Clear;
    }
    match status {
        "working" | "blocked" if turn.seen_working => TurnStep::Keep,
        "working" | "blocked" => TurnStep::MarkWorking,
        "idle" | "done" if turn.seen_working => TurnStep::Clear,
        _ if !turn.seen_working && age >= TURN_NEVER_WORKED_S => TurnStep::Clear,
        _ => TurnStep::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(started_unix: u64, seen_working: bool) -> Turn {
        Turn {
            source: "wake".into(),
            id: "7".into(),
            started_unix,
            coordinator_pane: "w1:p1".into(),
            seen_working,
        }
    }

    #[test]
    fn advance_steps_through_every_transition() {
        let fresh = turn(1000, false);
        assert_eq!(
            advance(&fresh, "idle", 1005),
            TurnStep::Keep,
            "not started yet"
        );
        assert_eq!(advance(&fresh, "working", 1005), TurnStep::MarkWorking);
        assert_eq!(advance(&fresh, "blocked", 1100), TurnStep::MarkWorking);
        assert_eq!(
            advance(&fresh, "idle", 1000 + TURN_NEVER_WORKED_S),
            TurnStep::Clear,
            "never worked: swallowed or instant"
        );
        let busy = turn(1000, true);
        assert_eq!(advance(&busy, "working", 1100), TurnStep::Keep);
        assert_eq!(advance(&busy, "blocked", 1100), TurnStep::Keep);
        assert_eq!(advance(&busy, "idle", 1100), TurnStep::Clear);
        assert_eq!(advance(&busy, "done", 1100), TurnStep::Clear);
        assert_eq!(
            advance(&busy, "working", 1000 + TURN_HARD_EXPIRY_S),
            TurnStep::Clear,
            "hard expiry wins over working"
        );
        assert_eq!(advance(&busy, "working", 0), TurnStep::Keep, "clock skew");
    }

    #[test]
    fn markers_round_trip_and_expire() {
        let dir = super::super::test_dir("turn");
        assert_eq!(read_live(&dir, 1000), None);
        clear(&dir); // absent: no-op
        let marker = turn(1000, false);
        write(&dir, &marker).unwrap();
        assert_eq!(read_live(&dir, 1000), Some(marker.clone()));
        assert_eq!(read_live(&dir, 1000 + TURN_HARD_EXPIRY_S - 1), Some(marker));
        assert_eq!(read_live(&dir, 1000 + TURN_HARD_EXPIRY_S), None);
        // A marker written before `seen_working` existed still parses.
        std::fs::write(
            turn_path(&dir),
            r#"{"source":"message","id":"m1","started_unix":5,"coordinator_pane":"w1:p1"}"#,
        )
        .unwrap();
        assert!(read_live(&dir, 10).is_some_and(|t| !t.seen_working));
        std::fs::write(turn_path(&dir), "not json").unwrap();
        assert_eq!(read_live(&dir, 10), None);
        clear(&dir);
        assert!(!turn_path(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
