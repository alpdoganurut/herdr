//! Fork (companion launcher): the placement board.
//!
//! Every attached client shell's browser placement (`browser.placement.set`)
//! with the server's facts about that client (its terminal has the user's
//! focus; it is the foreground client; its surface shows this server: a
//! client that switched to another endpoint keeps its placement but
//! unshown), for the browser host's long poll
//! (`browser.placements`), and the host's last report on each placement
//! (`browser.placement_report`, shown by `browser.get` `placements`).
//!
//! Process-global like the hub: the headless server's event loop writes the
//! placements and facts (never blocking: one short lock), connection threads
//! read and wait. `rev` grows with every change; `boot_id` is new with each
//! server process, so a host that outlives a live handoff starts over. A
//! client's placement and the host's report on it leave with the client.
//!
//! Fork (compact bar): the host's asks on a placement (promote, restore,
//! close; `browser.placement_action`) wait here, numbered, for the headless
//! server's render pass that relays each to the client owning it.
//!
//! Fork (browser keys): the host's cycle key (`browser.view_cycle`) waits
//! here the same way, for the client owning the placement the host covers,
//! else the foreground client (the board keeps which one that is).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::api::schema::{
    BrowserPlacementAction, BrowserPlacementInfo, BrowserPlacementKind, BrowserPlacementSpec,
    BrowserPlacementStatus,
};

/// The longest a `browser.placements` wait may block.
pub const MAX_WAIT: Duration = Duration::from_millis(25_000);
/// How often a waiter wakes to check the server is still running.
const WAIT_SLICE: Duration = Duration::from_millis(250);
/// Field caps (the client sends short values; a misbehaving one is cut).
const MAX_ID: usize = 64;
const MAX_TAB_ID: usize = 128;
const MAX_PIDS: usize = 16;
const MAX_STATE: usize = 32;
const MAX_REASON: usize = 300;
/// The report states a host may send; anything else is stored as `unknown`.
pub const REPORT_STATES: [&str; 6] = [
    "covering",
    "hidden",
    "moving",
    "unbound",
    "unsupported",
    "elsewhere",
];

/// Fork (compact bar): the asks kept for the relay (oldest dropped).
pub const ACTION_CAP: usize = 16;

/// Fork (compact bar): one host ask on a placement, for its client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementActionRecord {
    /// From 1, per server process.
    pub revision: u64,
    /// The server's id of the client that owns the placement.
    pub client_id: u64,
    /// The placement's id and herdr tab when it was asked.
    pub id: String,
    pub tab_id: String,
    pub action: BrowserPlacementAction,
}

/// Fork (browser keys): the cycles kept for the relay (oldest dropped).
pub const VIEW_CYCLE_CAP: usize = 16;

/// Fork (browser keys): one cycle-key press in the host, for its client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewCycleRecord {
    /// From 1, per server process.
    pub revision: u64,
    /// The server's id of the client that takes it.
    pub client_id: u64,
    /// The herdr tab the host showed when pressed.
    pub tab_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    spec: BrowserPlacementSpec,
    facts: ClientFacts,
    updated_unix: u64,
}

/// The server's facts about a client with a placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientFacts {
    /// Its terminal has the user's focus (outer focus).
    pub focused: bool,
    /// The foreground client.
    pub foreground: bool,
    /// Its surface shows this server (else the placement is not shown).
    pub surface_active: bool,
}

#[derive(Debug)]
struct Board {
    boot_id: String,
    rev: u64,
    /// By the server's client id.
    entries: BTreeMap<u64, Entry>,
    /// By placement id.
    reports: HashMap<String, BrowserPlacementStatus>,
    /// Fork (compact bar): the host's asks, at most [`ACTION_CAP`].
    actions: VecDeque<PlacementActionRecord>,
    action_revision: u64,
    /// Fork (compact bar): the clients whose handshake says they take the
    /// asks (`EndpointClientHello.placement_actions`); an older client
    /// ignores them, so they are refused for it.
    takes_actions: HashSet<u64>,
    /// Fork (browser keys): the clients whose handshake says they take the
    /// host's cycle key (`EndpointClientHello.view_cycle`).
    takes_view_cycle: HashSet<u64>,
    /// Fork (browser keys): the host's cycle-key presses, at most
    /// [`VIEW_CYCLE_CAP`].
    view_cycles: VecDeque<ViewCycleRecord>,
    view_cycle_revision: u64,
    /// Fork (browser keys): the server's foreground client, with or
    /// without a placement (the cycle's target when the host covers none).
    foreground: Option<u64>,
}

/// Fork (compact bar): why an ask was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionRefused {
    /// No client holds the placement.
    NotFound,
    /// The owning client predates placement actions.
    Unsupported,
}

/// See the module docs.
#[derive(Debug)]
pub struct PlacementBoard {
    board: Mutex<Board>,
    changed: Condvar,
}

/// The process's board.
pub fn board() -> &'static PlacementBoard {
    static BOARD: OnceLock<PlacementBoard> = OnceLock::new();
    BOARD.get_or_init(PlacementBoard::new)
}

/// What a read of the board answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementSnapshot {
    pub boot_id: String,
    pub rev: u64,
    pub placements: Vec<BrowserPlacementInfo>,
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_string(),
        None => text.to_string(),
    }
}

fn sanitize(mut spec: BrowserPlacementSpec) -> BrowserPlacementSpec {
    spec.id = clip(spec.id.trim(), MAX_ID);
    spec.tab_id = clip(spec.tab_id.trim(), MAX_TAB_ID);
    spec.pids.retain(|pid| *pid > 0);
    spec.pids.truncate(MAX_PIDS);
    spec
}

fn boot_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{:x}", std::process::id(), nanos)
}

impl Default for PlacementBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl PlacementBoard {
    pub fn new() -> Self {
        Self {
            board: Mutex::new(Board {
                boot_id: boot_id(),
                rev: 0,
                entries: BTreeMap::new(),
                reports: HashMap::new(),
                actions: VecDeque::new(),
                action_revision: 0,
                takes_actions: HashSet::new(),
                takes_view_cycle: HashSet::new(),
                view_cycles: VecDeque::new(),
                view_cycle_revision: 0,
                foreground: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Board> {
        // A panic while holding the lock leaves plain data behind; keep going.
        self.board
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn bump(&self, board: &mut Board) {
        board.rev = board.rev.saturating_add(1);
        self.changed.notify_all();
    }

    /// Store (or, with `None`, clear) client `client_id`'s placement with
    /// the server's facts about it. Returns whether the board changed. A
    /// placement without an id or a tab, or of an unknown kind, clears.
    pub fn set(
        &self,
        client_id: u64,
        spec: Option<BrowserPlacementSpec>,
        facts: ClientFacts,
        now_unix: u64,
    ) -> bool {
        let spec = spec.map(sanitize).filter(|spec| {
            !spec.id.is_empty()
                && !spec.tab_id.is_empty()
                && spec.kind != BrowserPlacementKind::Unknown
        });
        let mut board = self.lock();
        let Some(spec) = spec else {
            return self.remove_locked(&mut board, client_id);
        };
        if let Some(entry) = board.entries.get(&client_id) {
            if entry.spec == spec && entry.facts == facts {
                return false;
            }
        }
        // A new id from the same client (a restarted client) takes the old
        // id's report with it.
        let old_id = board
            .entries
            .get(&client_id)
            .map(|entry| entry.spec.id.clone())
            .filter(|old| *old != spec.id);
        if let Some(old_id) = old_id {
            board.reports.remove(&old_id);
        }
        board.entries.insert(
            client_id,
            Entry {
                spec,
                facts,
                updated_unix: now_unix,
            },
        );
        self.bump(&mut board);
        true
    }

    /// Refresh the facts of the clients on the board. Returns whether
    /// anything changed.
    pub fn set_facts(&self, facts: &[(u64, ClientFacts)]) -> bool {
        let mut board = self.lock();
        let mut changed = false;
        for (client_id, facts) in facts {
            if let Some(entry) = board.entries.get_mut(client_id) {
                if entry.facts != *facts {
                    entry.facts = *facts;
                    changed = true;
                }
            }
        }
        if changed {
            self.bump(&mut board);
        }
        changed
    }

    /// The client went away: its placement and the report on it go too.
    pub fn remove(&self, client_id: u64) -> bool {
        let mut board = self.lock();
        board.takes_actions.remove(&client_id);
        board.takes_view_cycle.remove(&client_id);
        if board.foreground == Some(client_id) {
            board.foreground = None;
        }
        self.remove_locked(&mut board, client_id)
    }

    /// Fork (compact bar): client `client_id`'s handshake said whether it
    /// takes the host's placement actions. Returns whether the board changed.
    pub fn set_takes_actions(&self, client_id: u64, takes: bool) -> bool {
        let mut board = self.lock();
        let changed = if takes {
            board.takes_actions.insert(client_id)
        } else {
            board.takes_actions.remove(&client_id)
        };
        if changed && board.entries.contains_key(&client_id) {
            self.bump(&mut board);
        }
        changed
    }

    /// Fork (browser keys): client `client_id`'s handshake said whether it
    /// takes the host's cycle key. Returns whether that changed.
    pub fn set_takes_view_cycle(&self, client_id: u64, takes: bool) -> bool {
        let mut board = self.lock();
        if takes {
            board.takes_view_cycle.insert(client_id)
        } else {
            board.takes_view_cycle.remove(&client_id)
        }
    }

    /// Fork (browser keys): the server's foreground client now (no
    /// placement needed; not part of `browser.placements`, so no `rev`).
    pub fn set_foreground(&self, client_id: Option<u64>) {
        self.lock().foreground = client_id;
    }

    fn remove_locked(&self, board: &mut Board, client_id: u64) -> bool {
        let Some(entry) = board.entries.remove(&client_id) else {
            return false;
        };
        board.reports.remove(&entry.spec.id);
        self.bump(board);
        true
    }

    /// The clients that have a placement (usually none or one).
    pub fn client_ids(&self) -> Vec<u64> {
        self.lock().entries.keys().copied().collect()
    }

    fn snapshot_locked(board: &Board) -> PlacementSnapshot {
        PlacementSnapshot {
            boot_id: board.boot_id.clone(),
            rev: board.rev,
            placements: board
                .entries
                .iter()
                .map(|(client_id, entry)| {
                    let mut placement = entry.spec.clone();
                    placement.shown &= entry.facts.surface_active;
                    BrowserPlacementInfo {
                        placement,
                        focused: entry.facts.focused,
                        foreground: entry.facts.foreground,
                        updated_unix: entry.updated_unix,
                        placement_actions: board.takes_actions.contains(client_id),
                    }
                })
                .collect(),
        }
    }

    /// Every placement, at once when the board is newer than `since_rev`
    /// (or it is `None`), else after the next change, `timeout` (capped at
    /// [`MAX_WAIT`]) or the server stopping (`running` false). A
    /// `since_rev` this board never reached is another server process's (a
    /// host that outlived a live handoff): at once too, with the new
    /// `boot_id`.
    pub fn wait(
        &self,
        since_rev: Option<u64>,
        timeout: Duration,
        running: &AtomicBool,
    ) -> PlacementSnapshot {
        let deadline = Instant::now() + timeout.min(MAX_WAIT);
        let mut board = self.lock();
        loop {
            let newer = since_rev.is_none_or(|since| board.rev != since);
            let now = Instant::now();
            if newer || now >= deadline || !running.load(Ordering::Relaxed) {
                return Self::snapshot_locked(&board);
            }
            let slice = deadline.saturating_duration_since(now).min(WAIT_SLICE);
            board = match self.changed.wait_timeout(board, slice) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// The host's report on placement `id`; ignored for an id the board
    /// does not hold (the client left). Returns whether it changed.
    pub fn report(&self, id: &str, state: &str, reason: Option<&str>) -> bool {
        let state = state.trim();
        let state = if REPORT_STATES.contains(&state) {
            state.to_string()
        } else {
            "unknown".to_string()
        };
        let status = BrowserPlacementStatus {
            id: clip(id.trim(), MAX_ID),
            state: clip(&state, MAX_STATE),
            reason: reason
                .map(|reason| clip(reason.trim(), MAX_REASON))
                .filter(|reason| !reason.is_empty()),
        };
        let mut board = self.lock();
        if !board
            .entries
            .values()
            .any(|entry| entry.spec.id == status.id)
        {
            return false;
        }
        if board.reports.get(&status.id) == Some(&status) {
            return false;
        }
        board.reports.insert(status.id.clone(), status);
        true
    }

    /// Fork (compact bar): the host asks the client owning placement `id`
    /// for `action`. Returns the ask's revision; refused for an id the board
    /// does not hold (the client left, or never published it) and for a
    /// client that predates placement actions (it would ignore the ask).
    pub fn queue_action(
        &self,
        id: &str,
        action: BrowserPlacementAction,
    ) -> Result<u64, ActionRefused> {
        let id = clip(id.trim(), MAX_ID);
        let mut board = self.lock();
        let (client_id, tab_id) = board
            .entries
            .iter()
            .find(|(_, entry)| entry.spec.id == id)
            .map(|(client_id, entry)| (*client_id, entry.spec.tab_id.clone()))
            .ok_or(ActionRefused::NotFound)?;
        if !board.takes_actions.contains(&client_id) {
            return Err(ActionRefused::Unsupported);
        }
        board.action_revision = board.action_revision.saturating_add(1);
        let revision = board.action_revision;
        board.actions.push_back(PlacementActionRecord {
            revision,
            client_id,
            id,
            tab_id,
            action,
        });
        while board.actions.len() > ACTION_CAP {
            board.actions.pop_front();
        }
        Ok(revision)
    }

    /// Fork (browser keys): the host's cycle key, for the client owning
    /// placement `id` when the board holds it, else the foreground client.
    /// Returns the press's revision; refused `NotFound` with no client to
    /// take it, `Unsupported` for a client that predates the relay.
    pub fn queue_view_cycle(
        &self,
        id: Option<&str>,
        tab_id: Option<&str>,
    ) -> Result<u64, ActionRefused> {
        let id = id
            .map(|id| clip(id.trim(), MAX_ID))
            .filter(|id| !id.is_empty());
        let tab_id = tab_id
            .map(|tab_id| clip(tab_id.trim(), MAX_TAB_ID))
            .filter(|tab_id| !tab_id.is_empty());
        let mut board = self.lock();
        let owner = id.and_then(|id| {
            board
                .entries
                .iter()
                .find(|(_, entry)| entry.spec.id == id)
                .map(|(client_id, _)| *client_id)
        });
        let client_id = owner.or(board.foreground).ok_or(ActionRefused::NotFound)?;
        if !board.takes_view_cycle.contains(&client_id) {
            return Err(ActionRefused::Unsupported);
        }
        board.view_cycle_revision = board.view_cycle_revision.saturating_add(1);
        let revision = board.view_cycle_revision;
        board.view_cycles.push_back(ViewCycleRecord {
            revision,
            client_id,
            tab_id,
        });
        while board.view_cycles.len() > VIEW_CYCLE_CAP {
            board.view_cycles.pop_front();
        }
        Ok(revision)
    }

    /// Fork (browser keys): the newest cycle's revision (0: none yet).
    pub fn view_cycle_revision(&self) -> u64 {
        self.lock().view_cycle_revision
    }

    /// Fork (browser keys): client `client_id`'s kept cycles above revision
    /// `after`, oldest first.
    pub fn view_cycles_for(&self, client_id: u64, after: u64) -> Vec<ViewCycleRecord> {
        self.lock()
            .view_cycles
            .iter()
            .filter(|record| record.client_id == client_id && record.revision > after)
            .cloned()
            .collect()
    }

    /// Fork (compact bar): the newest ask's revision (0: none yet).
    pub fn action_revision(&self) -> u64 {
        self.lock().action_revision
    }

    /// Fork (compact bar): client `client_id`'s kept asks above revision
    /// `after`, oldest first.
    pub fn actions_for(&self, client_id: u64, after: u64) -> Vec<PlacementActionRecord> {
        self.lock()
            .actions
            .iter()
            .filter(|record| record.client_id == client_id && record.revision > after)
            .cloned()
            .collect()
    }

    /// The host's reports on the placements the board holds, by id.
    pub fn statuses(&self) -> Vec<BrowserPlacementStatus> {
        let board = self.lock();
        let mut statuses: Vec<_> = board.reports.values().cloned().collect();
        statuses.sort_by(|a, b| a.id.cmp(&b.id));
        statuses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{BrowserCellRect, BrowserGridSize};

    impl PlacementBoard {
        fn snapshot(&self) -> PlacementSnapshot {
            Self::snapshot_locked(&self.lock())
        }
    }

    fn facts(focused: bool, foreground: bool) -> ClientFacts {
        ClientFacts {
            focused,
            foreground,
            surface_active: true,
        }
    }

    fn spec(id: &str, tab: &str) -> BrowserPlacementSpec {
        BrowserPlacementSpec {
            id: id.into(),
            kind: BrowserPlacementKind::Side,
            tab_id: tab.into(),
            area: BrowserCellRect {
                x: 60,
                y: 1,
                width: 40,
                height: 30,
            },
            grid: BrowserGridSize {
                cols: 100,
                rows: 31,
            },
            shown: true,
            ..Default::default()
        }
    }

    /// Fork (compact bar): an ask is kept for the client that owns the
    /// placement, numbered; an unknown id is refused; the oldest go past
    /// the cap.
    #[test]
    fn a_placement_action_is_kept_for_the_owning_client() {
        let board = PlacementBoard::new();
        assert_eq!(
            board.queue_action("nobody", BrowserPlacementAction::Close),
            Err(ActionRefused::NotFound)
        );
        assert!(board.set(7, Some(spec("pl1", "t1")), facts(true, true), 100));
        assert!(board.set(8, Some(spec("pl2", "t2")), facts(false, false), 100));
        // a client whose handshake did not say it takes them: refused
        assert_eq!(
            board.queue_action("pl1", BrowserPlacementAction::Promote),
            Err(ActionRefused::Unsupported)
        );
        assert!(!board.snapshot().placements[0].placement_actions);
        board.set_takes_actions(7, true);
        board.set_takes_actions(8, true);
        assert!(board
            .snapshot()
            .placements
            .iter()
            .all(|p| p.placement_actions));
        assert_eq!(
            board.queue_action(" pl1 ", BrowserPlacementAction::Promote),
            Ok(1)
        );
        assert_eq!(
            board.queue_action("pl2", BrowserPlacementAction::Close),
            Ok(2)
        );
        let mine = board.actions_for(7, 0);
        assert_eq!(mine.len(), 1);
        assert_eq!(
            (
                mine[0].revision,
                mine[0].id.as_str(),
                mine[0].tab_id.as_str()
            ),
            (1, "pl1", "t1")
        );
        assert_eq!(mine[0].action, BrowserPlacementAction::Promote);
        assert!(board.actions_for(7, 1).is_empty());
        assert_eq!(
            board.actions_for(8, 0)[0].action,
            BrowserPlacementAction::Close
        );
        for _ in 0..ACTION_CAP {
            assert!(board
                .queue_action("pl1", BrowserPlacementAction::Restore)
                .is_ok());
        }
        assert_eq!(board.action_revision(), 2 + ACTION_CAP as u64);
        assert!(board.actions_for(8, 0).is_empty(), "dropped past the cap");
        assert_eq!(board.actions_for(7, 0).len(), ACTION_CAP);
    }

    /// Fork (browser keys): a cycle goes to the client owning the covered
    /// placement, else to the foreground client (no placement needed);
    /// nobody to take it, or a client that predates the relay, is refused;
    /// a client that leaves stops being the foreground one.
    #[test]
    fn a_view_cycle_goes_to_the_placement_owner_else_the_foreground_client() {
        let board = PlacementBoard::new();
        assert_eq!(
            board.queue_view_cycle(None, Some("t1")),
            Err(ActionRefused::NotFound)
        );
        board.set_foreground(Some(5));
        assert_eq!(
            board.queue_view_cycle(None, Some("t1")),
            Err(ActionRefused::Unsupported),
            "a client without the handshake flag"
        );
        board.set_takes_view_cycle(5, true);
        board.set_takes_view_cycle(8, true);
        assert_eq!(board.queue_view_cycle(None, Some(" t1 ")), Ok(1));
        assert!(board.set(8, Some(spec("pl2", "t2")), facts(false, false), 100));
        // the covered placement's client, though another is in front
        assert_eq!(board.queue_view_cycle(Some("pl2"), Some("t2")), Ok(2));
        // a placement the board no longer holds: the foreground client
        assert_eq!(board.queue_view_cycle(Some("pl-gone"), None), Ok(3));
        let front = board.view_cycles_for(5, 0);
        assert_eq!(
            front
                .iter()
                .map(|r| (r.revision, r.tab_id.clone()))
                .collect::<Vec<_>>(),
            [(1, Some("t1".to_string())), (3, None)]
        );
        let owner = board.view_cycles_for(8, 0);
        assert_eq!(owner.len(), 1);
        assert_eq!(
            (owner[0].revision, owner[0].tab_id.as_deref()),
            (2, Some("t2"))
        );
        assert!(board.view_cycles_for(5, 3).is_empty());
        assert_eq!(board.view_cycle_revision(), 3);
        // placement actions are a separate queue
        assert!(board.actions_for(8, 0).is_empty());
        for _ in 0..VIEW_CYCLE_CAP {
            assert!(board.queue_view_cycle(Some("pl2"), None).is_ok());
        }
        assert!(
            board.view_cycles_for(5, 0).is_empty(),
            "dropped past the cap"
        );
        // the foreground client leaves: nobody to take it
        board.remove(5);
        assert_eq!(
            board.queue_view_cycle(None, None),
            Err(ActionRefused::NotFound)
        );
    }

    #[test]
    fn a_wait_answers_at_once_when_newer_and_blocks_until_a_change() {
        let board = std::sync::Arc::new(PlacementBoard::new());
        let running = std::sync::Arc::new(AtomicBool::new(true));
        // nothing seen yet: at once, even on an empty board
        let first = board.wait(None, Duration::from_secs(5), &running);
        assert_eq!(first.rev, 0);
        assert!(first.placements.is_empty());
        // up to date: a short wait times out with the same rev
        let started = Instant::now();
        let same = board.wait(Some(0), Duration::from_millis(120), &running);
        assert_eq!(same.rev, 0);
        assert!(started.elapsed() >= Duration::from_millis(100));
        // a change wakes a waiter long before its timeout
        let waiter = {
            let board = board.clone();
            let running = running.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                let snapshot = board.wait(Some(0), Duration::from_secs(10), &running);
                (snapshot, started.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(80));
        assert!(board.set(7, Some(spec("pl1", "t1")), facts(true, true), 100));
        let (snapshot, waited) = waiter.join().expect("waiter");
        assert_eq!(snapshot.rev, 1);
        assert_eq!(snapshot.placements.len(), 1);
        assert!(snapshot.placements[0].focused && snapshot.placements[0].foreground);
        assert!(waited < Duration::from_secs(5), "woke at once: {waited:?}");
        // the same placement again changes nothing
        assert!(!board.set(7, Some(spec("pl1", "t1")), facts(true, true), 101));
        assert_eq!(board.snapshot().rev, 1);
        // a rev from another server process (ahead of this board) answers at
        // once: a host that outlived a live handoff starts over
        let started = Instant::now();
        let other_boot = board.wait(Some(57), Duration::from_secs(10), &running);
        assert_eq!(other_boot.rev, 1);
        assert!(started.elapsed() < Duration::from_secs(2));
        // the server stopping ends a wait
        running.store(false, Ordering::Relaxed);
        let started = Instant::now();
        board.wait(Some(1), Duration::from_secs(10), &running);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_disconnected_client_takes_its_placement_with_it() {
        let board = PlacementBoard::new();
        board.set(1, Some(spec("pa", "t1")), facts(false, true), 1);
        board.set(2, Some(spec("pb", "t2")), facts(true, false), 1);
        assert!(board.report("pa", "covering", None));
        assert!(board.report("pb", "unbound", Some("click the band once")));
        // an unknown state reads as unknown; a report for a gone id is dropped
        assert!(!board.report("nobody", "covering", None));
        assert_eq!(board.statuses().len(), 2);
        let rev = board.snapshot().rev;
        assert!(board.remove(1));
        let snapshot = board.snapshot();
        assert_eq!(snapshot.rev, rev + 1);
        assert_eq!(snapshot.placements.len(), 1);
        assert_eq!(snapshot.placements[0].placement.id, "pb");
        assert_eq!(
            board.statuses(),
            vec![BrowserPlacementStatus {
                id: "pb".into(),
                state: "unbound".into(),
                reason: Some("click the band once".into()),
            }]
        );
        // clearing by `None` is a removal too; removing twice changes nothing
        assert!(board.set(2, None, facts(false, false), 2));
        assert!(!board.remove(2));
        assert!(board.statuses().is_empty());
    }

    #[test]
    fn facts_change_the_board_only_when_they_differ() {
        let board = PlacementBoard::new();
        board.set(3, Some(spec("pc", "t1")), facts(false, false), 1);
        let rev = board.snapshot().rev;
        assert!(!board.set_facts(&[(3, facts(false, false)), (9, facts(true, true))]));
        assert_eq!(board.snapshot().rev, rev);
        assert!(board.set_facts(&[(3, facts(true, false))]));
        let snapshot = board.snapshot();
        assert_eq!(snapshot.rev, rev + 1);
        assert!(snapshot.placements[0].focused);
        assert!(snapshot.placements[0].placement.shown);
        assert_eq!(board.client_ids(), vec![3]);
        // a client that shows another endpoint keeps its placement, unshown
        assert!(board.set_facts(&[(
            3,
            ClientFacts {
                surface_active: false,
                ..facts(true, false)
            }
        )]));
        assert!(!board.snapshot().placements[0].placement.shown);
    }

    #[test]
    fn a_placement_without_an_id_or_of_an_unknown_kind_clears() {
        let board = PlacementBoard::new();
        board.set(4, Some(spec("pd", "t1")), facts(false, false), 1);
        let mut unknown = spec("pd", "t1");
        unknown.kind = BrowserPlacementKind::Unknown;
        assert!(board.set(4, Some(unknown), facts(false, false), 2));
        assert!(board.snapshot().placements.is_empty());
        assert!(!board.set(4, Some(spec("", "t1")), facts(false, false), 3));
        let mut long = spec(&"x".repeat(200), "t1");
        long.pids = (1..40).collect();
        board.set(5, Some(long), facts(false, false), 4);
        let snapshot = board.snapshot();
        assert_eq!(snapshot.placements[0].placement.id.len(), MAX_ID);
        assert_eq!(snapshot.placements[0].placement.pids.len(), MAX_PIDS);
    }
}
