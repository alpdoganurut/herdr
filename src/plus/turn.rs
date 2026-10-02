//! The non-user-turn marker (`turn.json`): written before herdr+ itself
//! prompts the coordinator (a wake-up or an agent message), so the MCP write
//! tools can tell that turn was not started by the user. The watcher steps
//! the marker with [`advance`] each tick and clears it when the turn is over.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{live_path, turn_path, write_atomically};

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

/// Past this age a marker is over unless the watcher, which is running,
/// still sees the coordinator working or blocked in it.
pub const TURN_HARD_EXPIRY_S: u64 = 600;
pub const TURN_NEVER_WORKED_S: u64 = 60;
/// The absolute limit, whatever the status (a turn blocked overnight on a
/// prompt still counts as herdr+'s when the user answers it in the morning).
pub const TURN_SAFETY_S: u64 = 24 * 3600;
/// live.json younger than this means the watcher is running.
const WATCHER_FRESH_S: u64 = 15;
/// Lock taken by every read-modify-write of the marker (the watcher and the
/// MCP servers both write it).
const TURN_LOCK: &str = "turn";

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

fn read(dir: &Path) -> Option<Turn> {
    let bytes = std::fs::read(turn_path(dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether the watcher wrote live.json recently.
fn watcher_alive(dir: &Path, now: u64) -> bool {
    std::fs::read(live_path(dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|live| live["generated_unix"].as_u64())
        .is_some_and(|at| now.saturating_sub(at) < WATCHER_FRESH_S)
}

/// The marker, unless it is absent, unparseable or over. Past
/// [`TURN_HARD_EXPIRY_S`] it stays live only while the watcher runs (the
/// watcher clears it once the coordinator is idle, see [`advance`]); with no
/// watcher nothing else would clear it, so it expires there.
pub fn read_live(dir: &Path, now: u64) -> Option<Turn> {
    let turn = read(dir)?;
    let age = now.saturating_sub(turn.started_unix);
    let live = age < TURN_HARD_EXPIRY_S || (age < TURN_SAFETY_S && watcher_alive(dir, now));
    live.then_some(turn)
}

pub fn clear(dir: &Path) {
    if let Err(err) = std::fs::remove_file(turn_path(dir)) {
        if err.kind() != io::ErrorKind::NotFound {
            tracing::warn!("herdr+ cannot clear the turn marker: {err}");
        }
    }
}

/// The same turn: a rewrite (`seen_working`) keeps these.
fn same(a: &Turn, b: &Turn) -> bool {
    a.source == b.source && a.id == b.id && a.started_unix == b.started_unix
}

/// Write `turn` unless another marker is live; `false` when one is. The check
/// and the write happen under the turn lock, so two writers (a wake-up and
/// an agent message) cannot both take the coordinator's next turn.
pub fn write_if_absent(dir: &Path, turn: &Turn, now: u64) -> io::Result<bool> {
    let _lock = super::lock::exclusive(dir, TURN_LOCK)?;
    if read_live(dir, now).is_some() {
        return Ok(false);
    }
    write(dir, turn)?;
    Ok(true)
}

/// Remove the marker only while it still is `expected` (another writer may
/// have started a new turn since it was read).
pub fn clear_if(dir: &Path, expected: &Turn) {
    match super::lock::exclusive(dir, TURN_LOCK) {
        Ok(_lock) => {
            if read(dir).is_some_and(|turn| same(&turn, expected)) {
                clear(dir);
            }
        }
        Err(err) => tracing::warn!("herdr+ cannot lock the turn marker: {err}"),
    }
}

/// Record `seen_working` on the marker only while it still is `expected`.
pub fn mark_working_if(dir: &Path, expected: &Turn) -> io::Result<()> {
    let _lock = super::lock::exclusive(dir, TURN_LOCK)?;
    match read(dir) {
        Some(turn) if same(&turn, expected) && !turn.seen_working => write(
            dir,
            &Turn {
                seen_working: true,
                ..turn
            },
        ),
        _ => Ok(()),
    }
}

/// Pure: given the live turn and the coordinator's current status, the next
/// marker state. In order: past [`TURN_SAFETY_S`] the turn is over whatever
/// the status; past [`TURN_HARD_EXPIRY_S`] it is over unless the coordinator
/// is still working or blocked in it (a long or stalled herdr+ turn stays
/// guarded; `herdr plus coordinator clear-turn` is the user's way out);
/// `working` is recorded once (`Keep` when already seen, so the marker is not
/// rewritten every tick), and so is `blocked`: herdr+ only prompts an idle
/// coordinator, so blocked means the turn started and hit a prompt (a poll
/// may miss the working in between); after working, `idle`/`done` ends the turn; a turn
/// that never showed `working` within [`TURN_NEVER_WORKED_S`] was swallowed
/// or instant and ends too. Anything else (`blocked`, `unknown`, ...) keeps it.
pub fn advance(turn: &Turn, status: &str, now: u64) -> TurnStep {
    let age = now.saturating_sub(turn.started_unix);
    let running = matches!(status, "working" | "blocked");
    if age >= TURN_SAFETY_S || (age >= TURN_HARD_EXPIRY_S && !running) {
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
            TurnStep::Keep,
            "a long turn still working stays guarded"
        );
        assert_eq!(
            advance(&busy, "blocked", 1000 + 3 * TURN_HARD_EXPIRY_S),
            TurnStep::Keep,
            "a turn stalled on a prompt stays guarded"
        );
        assert_eq!(
            advance(&busy, "offline", 1000 + TURN_HARD_EXPIRY_S),
            TurnStep::Clear,
            "past the expiry, a coordinator not in a turn ends it"
        );
        assert_eq!(
            advance(&busy, "blocked", 1000 + TURN_SAFETY_S),
            TurnStep::Clear,
            "the safety limit wins over working"
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
        // With the watcher running it stays live past the expiry (the
        // watcher ends it), up to the safety limit.
        let late = 1000 + 2 * TURN_HARD_EXPIRY_S;
        std::fs::write(live_path(&dir), format!(r#"{{"generated_unix":{late}}}"#)).unwrap();
        assert!(read_live(&dir, late).is_some());
        assert_eq!(
            read_live(&dir, late + WATCHER_FRESH_S),
            None,
            "watcher gone"
        );
        std::fs::write(
            live_path(&dir),
            format!(r#"{{"generated_unix":{}}}"#, 1000 + TURN_SAFETY_S),
        )
        .unwrap();
        assert_eq!(read_live(&dir, 1000 + TURN_SAFETY_S), None);
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

    #[test]
    fn conditional_updates_leave_a_newer_turn_alone() {
        let dir = super::super::test_dir("turn-cas");
        let wake = turn(1000, false);
        assert!(write_if_absent(&dir, &wake, 1000).unwrap());
        let message = Turn {
            source: "message".into(),
            id: "m1".into(),
            ..turn(1001, false)
        };
        assert!(
            !write_if_absent(&dir, &message, 1001).unwrap(),
            "the wake turn is live"
        );
        mark_working_if(&dir, &wake).unwrap();
        assert!(read_live(&dir, 1002).is_some_and(|t| t.seen_working));
        // The watcher read the wake turn; meanwhile it ended and a message
        // turn started: neither a stale mark nor a stale clear touches it.
        clear(&dir);
        assert!(write_if_absent(&dir, &message, 1003).unwrap());
        mark_working_if(&dir, &wake).unwrap();
        clear_if(&dir, &wake);
        assert_eq!(read_live(&dir, 1004), Some(message.clone()));
        clear_if(&dir, &message);
        assert_eq!(read_live(&dir, 1004), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
