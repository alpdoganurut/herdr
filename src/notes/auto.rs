//! Automatic checkpoints (fork, `[notes] auto_checkpoints`): history that
//! needs no agent cooperation. Pure decisions over the pane's status edges
//! (`App::note_auto_checkpoints` feeds them), plus the git reads the notes
//! worker runs off the app thread.
//!
//! - a user turn: a `bookmark` `you: HH:MM` (the turn's start time), added
//!   when the turn ends so its anchor sits after the prompt and the reply
//!   (the agent writes its transcript's prompt line only after the status
//!   flips to working, so a turn-start anchor would show the turn before);
//! - a turn end whose pane repo got new commits since the turn started: a
//!   `milestone` naming them (HEAD read at turn start and end on the
//!   worker; `git log` only when HEAD moved);
//! - a pane blocked for [`BLOCKED_LONG`] or more, when it unblocks, and an
//!   agent that exits mid-turn (not parked by herdr): a `failure`.
//!
//! Every automatic checkpoint carries the tag [`super::recall::AUTO_TAG`]
//! (the info pane dims it; recall skips the prompt bookmarks) and counts
//! against its own hourly cap, never the agent's.
//!
//! Work per edge is O(1) map access; nothing here runs in a render path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) use crate::agents_model::turn::{EdgeStatus, TurnEdge};
use crate::api::schema::notes::CheckpointKind;

/// A blocked spell at least this long is recorded when it ends.
pub(crate) const BLOCKED_LONG: Duration = Duration::from_secs(10 * 60);
/// Commits a milestone names at most.
pub(crate) const MAX_COMMITS: usize = 10;
/// Commits older than the turn start by more than this are not the turn's
/// (a checkout of an older branch moves HEAD too).
const COMMIT_CLOCK_SLACK_S: u64 = 120;
/// Panes tracked before the oldest-touched are dropped.
const MAX_TRACKED: usize = 512;

/// One pane state update as the tracker sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeFacts {
    pub previous: EdgeStatus,
    pub next: EdgeStatus,
    pub turn: TurnEdge,
    /// The agent process went away in this update.
    pub released: bool,
    /// It was working or blocked when it went (its final status).
    pub released_mid_turn: bool,
    /// herdr parked or relaunches it (suspend, restart): not unexpected.
    pub parked: bool,
    /// The pane's notes key now (`None`: no agent session known yet).
    pub key: Option<String>,
    /// The pane's working directory.
    pub cwd: Option<PathBuf>,
    /// The agent's label (`claude`, `codex`).
    pub agent: Option<String>,
    pub now_unix: u64,
}

/// A checkpoint to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutoCheckpoint {
    pub key: String,
    pub kind: CheckpointKind,
    pub title: String,
    pub detail: Option<String>,
}

/// A HEAD read for the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitProbe {
    pub pane: String,
    pub turn: u64,
    pub cwd: PathBuf,
    /// At a turn end: the HEAD at its start (the result lists the commits
    /// after it). `None`: a turn start's read.
    pub since: Option<String>,
    /// The turn start (unix seconds), for the commit-time filter.
    pub turn_started_unix: u64,
}

/// What the tracker asks for after an edge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AutoActions {
    pub checkpoints: Vec<AutoCheckpoint>,
    pub probe: Option<GitProbe>,
}

#[derive(Debug, Clone, Default)]
struct PaneTrack {
    /// The notes key of the running (or last) turn.
    key: Option<String>,
    agent: Option<String>,
    turn: u64,
    turn_started_unix: u64,
    cwd: Option<PathBuf>,
    /// HEAD at the turn start (or after the last milestone in it).
    head: Option<String>,
    blocked_since: Option<Instant>,
    touched: Option<Instant>,
    /// The running user turn's bookmark title, added when the turn ends.
    pending_prompt: Option<String>,
}

/// The per-pane memory behind the automatic checkpoints.
#[derive(Debug, Default)]
pub(crate) struct AutoTracker {
    panes: HashMap<String, PaneTrack>,
}

/// `you: HH:MM`.
pub(crate) fn prompt_title(hhmm: &str) -> String {
    format!("you: {hhmm}")
}

/// The running user turn's bookmark, keyed by the turn's session as known
/// now (a fresh launch learns it during its first turn).
fn take_prompt_bookmark(track: &mut PaneTrack) -> Option<AutoCheckpoint> {
    let title = track.pending_prompt.take()?;
    Some(AutoCheckpoint {
        key: track.key.clone()?,
        kind: CheckpointKind::Bookmark,
        title,
        detail: None,
    })
}

fn minutes(duration: Duration) -> u64 {
    duration.as_secs().div_ceil(60)
}

impl AutoTracker {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.panes.len()
    }

    fn track(&mut self, pane: &str, now: Instant) -> &mut PaneTrack {
        if !self.panes.contains_key(pane) && self.panes.len() >= MAX_TRACKED {
            let oldest = self
                .panes
                .iter()
                .min_by_key(|(_, track)| track.touched)
                .map(|(pane, _)| pane.clone());
            if let Some(oldest) = oldest {
                self.panes.remove(&oldest);
            }
        }
        let track = self.panes.entry(pane.to_owned()).or_default();
        track.touched = Some(now);
        track
    }

    /// The actions for one pane state update. `hhmm` is the local time of
    /// the edge for the prompt bookmark's title.
    pub(crate) fn on_edge(
        &mut self,
        pane: &str,
        facts: EdgeFacts,
        hhmm: &str,
        now: Instant,
    ) -> AutoActions {
        let mut actions = AutoActions::default();
        let track = self.track(pane, now);
        if facts.agent.is_some() {
            track.agent = facts.agent.clone();
        }
        if facts.key.is_some() {
            track.key = facts.key.clone();
        }
        // A long blocked spell, recorded when it ends.
        if facts.next == EdgeStatus::Blocked && facts.previous != EdgeStatus::Blocked {
            track.blocked_since = Some(now);
        } else if facts.previous == EdgeStatus::Blocked && facts.next != EdgeStatus::Blocked {
            if let (Some(since), Some(key)) = (track.blocked_since.take(), track.key.clone()) {
                let blocked = now.saturating_duration_since(since);
                if blocked >= BLOCKED_LONG && !facts.parked {
                    actions.checkpoints.push(AutoCheckpoint {
                        key,
                        kind: CheckpointKind::Failure,
                        title: format!("blocked {} min waiting for you", minutes(blocked)),
                        detail: None,
                    });
                }
            }
        }
        if facts.released {
            actions.checkpoints.extend(take_prompt_bookmark(track));
            if facts.released_mid_turn && !facts.parked {
                if let Some(key) = track.key.clone() {
                    let agent = track.agent.clone().unwrap_or_else(|| "the agent".into());
                    actions.checkpoints.push(AutoCheckpoint {
                        key,
                        kind: CheckpointKind::Failure,
                        title: format!("{agent} exited mid-turn"),
                        detail: Some(
                            "The agent process ended while it was working or blocked.".into(),
                        ),
                    });
                }
            }
            // The next agent in the pane starts afresh.
            *track = PaneTrack {
                touched: Some(now),
                ..PaneTrack::default()
            };
            return actions;
        }
        match facts.turn {
            TurnEdge::Started { user } => {
                // A turn whose end was never seen still gets its bookmark.
                actions.checkpoints.extend(take_prompt_bookmark(track));
                track.turn += 1;
                track.turn_started_unix = facts.now_unix;
                track.head = None;
                track.cwd = facts.cwd.clone();
                if let Some(cwd) = facts.cwd {
                    actions.probe = Some(GitProbe {
                        pane: pane.to_owned(),
                        turn: track.turn,
                        cwd,
                        since: None,
                        turn_started_unix: facts.now_unix,
                    });
                }
                if user {
                    track.pending_prompt = Some(prompt_title(hhmm));
                }
            }
            TurnEdge::Ended => {
                actions.checkpoints.extend(take_prompt_bookmark(track));
                if let (Some(head), Some(cwd)) = (track.head.clone(), track.cwd.clone()) {
                    actions.probe = Some(GitProbe {
                        pane: pane.to_owned(),
                        turn: track.turn,
                        cwd,
                        since: Some(head),
                        turn_started_unix: track.turn_started_unix,
                    });
                }
            }
            TurnEdge::None | TurnEdge::Bridged => {}
        }
        actions
    }

    /// A worker HEAD read came back: remember a turn start's HEAD; a turn
    /// end's new commits become a milestone.
    pub(crate) fn on_git(
        &mut self,
        probe: &GitProbe,
        head: Option<String>,
        commits: &[String],
    ) -> Option<AutoCheckpoint> {
        let track = self.panes.get_mut(&probe.pane)?;
        if track.turn != probe.turn {
            // A newer turn started meanwhile; its own read is on the way.
            return None;
        }
        match &probe.since {
            None => {
                if track.head.is_none() {
                    track.head = head;
                }
                None
            }
            Some(since) => {
                let head = head?;
                if *since == head {
                    return None;
                }
                // Later edges in this turn compare from here.
                track.head = Some(head);
                milestone(track.key.clone()?, commits)
            }
        }
    }
}

/// The milestone for `commits` (`<short sha>\t<subject>`, newest first).
fn milestone(key: String, commits: &[String]) -> Option<AutoCheckpoint> {
    let newest = commits.first()?;
    let subject = newest.split_once('\t').map_or(newest.as_str(), |(_, s)| s);
    let title = if commits.len() == 1 {
        format!("commit: {subject}")
    } else if commits.len() > MAX_COMMITS {
        format!("{MAX_COMMITS}+ commits: {subject}")
    } else {
        format!("{} commits: {subject}", commits.len())
    };
    let detail = commits
        .iter()
        .take(MAX_COMMITS)
        .map(|line| line.replacen('\t', " ", 1))
        .collect::<Vec<_>>()
        .join("\n");
    Some(AutoCheckpoint {
        key,
        kind: CheckpointKind::Milestone,
        title: crate::coordinator::one_line(&title, 120),
        detail: Some(detail),
    })
}

// ---------------------------------------------------------------------------
// Git reads (the notes worker's thread)

/// The HEAD commit of the repository `cwd` is in, from its files (no git
/// process): `None` outside a repository, on an unborn branch or with ref
/// storage these reads do not know (reftable).
pub(crate) fn read_head(cwd: &Path) -> Option<String> {
    let (git_dir, common_dir) = git_dirs(cwd)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let oid = match head.strip_prefix("ref: ") {
        Some(full_ref) => read_ref(&common_dir, full_ref.trim())?,
        None => head.to_owned(),
    };
    is_oid(&oid).then_some(oid)
}

/// Directory levels walked up looking for `.git`.
const MAX_DEPTH: usize = 64;

/// The git directory and the common directory (they differ in a linked
/// worktree) of the repository `cwd` is in: the nearest `.git` directory,
/// or a `.git` file's `gitdir:`, and that directory's `commondir`.
fn git_dirs(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot_git = cwd
        .ancestors()
        .take(MAX_DEPTH)
        .map(|dir| dir.join(".git"))
        .find(|candidate| candidate.exists())?;
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        let target = PathBuf::from(target);
        if target.is_absolute() {
            target
        } else {
            dot_git.parent()?.join(target)
        }
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let common = PathBuf::from(text.trim());
            if common.is_absolute() {
                common
            } else {
                git_dir.join(common)
            }
        }
        Err(_) => git_dir.clone(),
    };
    Some((git_dir, common_dir))
}

fn is_oid(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// A ref's commit: the loose file, else `packed-refs`.
fn read_ref(common_dir: &Path, full_ref: &str) -> Option<String> {
    if full_ref.contains("..") || !full_ref.starts_with("refs/") {
        return None;
    }
    match std::fs::read_to_string(common_dir.join(full_ref)) {
        Ok(text) => return Some(text.trim().to_owned()).filter(|oid| !oid.is_empty()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    let packed = std::fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (oid, name) = line.trim().split_once(' ')?;
        (name == full_ref && !oid.starts_with('#') && !oid.starts_with('^')).then(|| oid.to_owned())
    })
}

/// The commits in `since..head` committed since the turn started (minus
/// [`COMMIT_CLOCK_SLACK_S`]), newest first, as `<short sha>\t<subject>`;
/// at most [`MAX_COMMITS`] + 1. One `git log`; empty on any failure.
pub(crate) fn commits_between(
    cwd: &Path,
    since: &str,
    head: &str,
    turn_started_unix: u64,
) -> Vec<String> {
    if !is_oid(since) || !is_oid(head) {
        return Vec::new();
    }
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "log",
            "--no-color",
            "--format=%h%x09%ct%x09%s",
            &format!("-n{}", MAX_COMMITS + 1),
            &format!("{since}..{head}"),
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_log(
        &String::from_utf8_lossy(&output.stdout),
        turn_started_unix.saturating_sub(COMMIT_CLOCK_SLACK_S),
    )
}

/// `%h\t%ct\t%s` lines committed at or after `not_before`, as `%h\t%s`.
fn parse_log(text: &str, not_before: u64) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let sha = parts.next()?.trim();
            let at: u64 = parts.next()?.trim().parse().ok()?;
            let subject = parts.next().unwrap_or("").trim();
            (!sha.is_empty() && at >= not_before).then(|| format!("{sha}\t{subject}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(previous: EdgeStatus, next: EdgeStatus, turn: TurnEdge) -> EdgeFacts {
        EdgeFacts {
            previous,
            next,
            turn,
            released: false,
            released_mid_turn: false,
            parked: false,
            key: Some("claude-s1".into()),
            cwd: Some(PathBuf::from("/repo")),
            agent: Some("claude".into()),
            now_unix: 1_000_000,
        }
    }

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn a_user_turn_bookmarks_its_start_time_when_it_ends_and_reads_head() {
        let mut tracker = AutoTracker::default();
        let now = Instant::now();
        let actions = tracker.on_edge(
            "w1:p1",
            facts(
                EdgeStatus::Idle,
                EdgeStatus::Working,
                TurnEdge::Started { user: true },
            ),
            "14:02",
            now,
        );
        assert!(actions.checkpoints.is_empty());
        let probe = actions.probe.unwrap();
        assert_eq!(probe.since, None);
        assert_eq!(probe.cwd, PathBuf::from("/repo"));
        let bookmark = AutoCheckpoint {
            key: "claude-s1".into(),
            kind: CheckpointKind::Bookmark,
            title: "you: 14:02".into(),
            detail: None,
        };
        // the end anchors it after the prompt and the reply
        let end = tracker.on_edge(
            "w1:p1",
            facts(EdgeStatus::Working, EdgeStatus::Idle, TurnEdge::Ended),
            "14:04",
            now,
        );
        assert_eq!(end.checkpoints, std::slice::from_ref(&bookmark));
        // a turn whose end was missed gets it at the next start, an exit at
        // the release
        let start = facts(
            EdgeStatus::Idle,
            EdgeStatus::Working,
            TurnEdge::Started { user: true },
        );
        let _ = tracker.on_edge("w1:p1", start.clone(), "14:02", now);
        let again = tracker.on_edge("w1:p1", start.clone(), "14:03", now);
        assert_eq!(again.checkpoints, std::slice::from_ref(&bookmark));
        let mut exit = facts(EdgeStatus::Working, EdgeStatus::Idle, TurnEdge::None);
        exit.released = true;
        let gone = tracker.on_edge("w1:p1", exit, "14:09", now);
        assert_eq!(
            gone.checkpoints,
            [AutoCheckpoint {
                title: "you: 14:03".into(),
                ..bookmark
            }]
        );
        // an agent's or a script's turn: no bookmark, still the HEAD read
        let actions = tracker.on_edge(
            "w1:p1",
            facts(
                EdgeStatus::Idle,
                EdgeStatus::Working,
                TurnEdge::Started { user: false },
            ),
            "14:05",
            now,
        );
        assert!(actions.checkpoints.is_empty());
        assert!(actions.probe.is_some());
        // no session known at the start: the one learned by the end keys it
        let mut unknown = facts(
            EdgeStatus::Idle,
            EdgeStatus::Working,
            TurnEdge::Started { user: true },
        );
        unknown.key = None;
        let mut fresh = AutoTracker::default();
        assert!(fresh
            .on_edge("w1:p2", unknown.clone(), "14:06", now)
            .checkpoints
            .is_empty());
        let learned = fresh.on_edge(
            "w1:p2",
            facts(EdgeStatus::Working, EdgeStatus::Idle, TurnEdge::Ended),
            "14:07",
            now,
        );
        assert_eq!(learned.checkpoints.len(), 1);
        assert_eq!(learned.checkpoints[0].title, "you: 14:06");
        // never known: nothing to key a bookmark on
        let mut never = AutoTracker::default();
        let _ = never.on_edge("w1:p3", unknown.clone(), "14:06", now);
        let mut end = facts(EdgeStatus::Working, EdgeStatus::Idle, TurnEdge::Ended);
        end.key = None;
        assert!(never
            .on_edge("w1:p3", end, "14:07", now)
            .checkpoints
            .is_empty());
    }

    #[test]
    fn new_commits_in_a_turn_become_one_milestone() {
        let mut tracker = AutoTracker::default();
        let now = Instant::now();
        let start = tracker
            .on_edge(
                "w1:p1",
                facts(
                    EdgeStatus::Idle,
                    EdgeStatus::Working,
                    TurnEdge::Started { user: true },
                ),
                "14:02",
                now,
            )
            .probe
            .unwrap();
        assert_eq!(tracker.on_git(&start, Some(SHA_A.into()), &[]), None);
        let end = tracker
            .on_edge(
                "w1:p1",
                facts(EdgeStatus::Working, EdgeStatus::Idle, TurnEdge::Ended),
                "14:09",
                now,
            )
            .probe
            .unwrap();
        assert_eq!(end.since.as_deref(), Some(SHA_A));
        let commits = vec![
            "bbbbbbb\tfix: the parser".to_string(),
            "ccccccc\tadd tests".into(),
        ];
        let milestone = tracker.on_git(&end, Some(SHA_B.into()), &commits).unwrap();
        assert_eq!(milestone.kind, CheckpointKind::Milestone);
        assert_eq!(milestone.key, "claude-s1");
        assert_eq!(milestone.title, "2 commits: fix: the parser");
        assert_eq!(
            milestone.detail.as_deref(),
            Some("bbbbbbb fix: the parser\nccccccc add tests")
        );
        // a flap's second end compares from the new HEAD: nothing twice
        let again = tracker
            .on_edge(
                "w1:p1",
                facts(EdgeStatus::Working, EdgeStatus::Idle, TurnEdge::Ended),
                "14:10",
                now,
            )
            .probe
            .unwrap();
        assert_eq!(again.since.as_deref(), Some(SHA_B));
        assert_eq!(tracker.on_git(&again, Some(SHA_B.into()), &[]), None);
        // HEAD moved without commits of this turn (a checkout): nothing
        assert_eq!(tracker.on_git(&again, Some(SHA_A.into()), &[]), None);
        // a stale read from an older turn is dropped
        let _ = tracker.on_edge(
            "w1:p1",
            facts(
                EdgeStatus::Idle,
                EdgeStatus::Working,
                TurnEdge::Started { user: false },
            ),
            "14:11",
            now,
        );
        assert_eq!(tracker.on_git(&end, Some(SHA_A.into()), &commits), None);
    }

    #[test]
    fn a_turn_end_before_the_start_read_came_back_asks_nothing() {
        let mut tracker = AutoTracker::default();
        let now = Instant::now();
        let _ = tracker.on_edge(
            "w1:p1",
            facts(
                EdgeStatus::Idle,
                EdgeStatus::Working,
                TurnEdge::Started { user: false },
            ),
            "14:02",
            now,
        );
        let end = tracker.on_edge(
            "w1:p1",
            facts(EdgeStatus::Working, EdgeStatus::Idle, TurnEdge::Ended),
            "14:02",
            now,
        );
        assert_eq!(end, AutoActions::default());
    }

    #[test]
    fn a_long_block_and_an_exit_mid_turn_are_failures_unless_herdr_parked_it() {
        let mut tracker = AutoTracker::default();
        let t0 = Instant::now();
        let _ = tracker.on_edge(
            "w1:p1",
            facts(EdgeStatus::Working, EdgeStatus::Blocked, TurnEdge::None),
            "14:00",
            t0,
        );
        let short = tracker.on_edge(
            "w1:p1",
            facts(EdgeStatus::Blocked, EdgeStatus::Working, TurnEdge::None),
            "14:01",
            t0 + Duration::from_secs(60),
        );
        assert!(short.checkpoints.is_empty());
        let _ = tracker.on_edge(
            "w1:p1",
            facts(EdgeStatus::Working, EdgeStatus::Blocked, TurnEdge::None),
            "14:02",
            t0,
        );
        let long = tracker.on_edge(
            "w1:p1",
            facts(EdgeStatus::Blocked, EdgeStatus::Working, TurnEdge::None),
            "14:14",
            t0 + BLOCKED_LONG + Duration::from_secs(90),
        );
        assert_eq!(long.checkpoints.len(), 1);
        assert_eq!(long.checkpoints[0].kind, CheckpointKind::Failure);
        assert_eq!(long.checkpoints[0].title, "blocked 12 min waiting for you");

        let mut exit = facts(EdgeStatus::Working, EdgeStatus::Unknown, TurnEdge::Ended);
        exit.released = true;
        exit.released_mid_turn = true;
        exit.key = None; // the session is gone: the turn's key is used
        let failed = tracker.on_edge("w1:p1", exit.clone(), "14:20", t0);
        assert_eq!(failed.checkpoints.len(), 1);
        assert_eq!(failed.checkpoints[0].title, "claude exited mid-turn");
        assert_eq!(failed.checkpoints[0].key, "claude-s1");
        assert_eq!(failed.probe, None);
        // parked by herdr (suspend, restart), or an idle exit: nothing
        let mut tracker = AutoTracker::default();
        let _ = tracker.on_edge(
            "w1:p1",
            facts(EdgeStatus::Idle, EdgeStatus::Working, TurnEdge::None),
            "14:00",
            t0,
        );
        let mut parked = exit.clone();
        parked.parked = true;
        assert!(tracker
            .on_edge("w1:p1", parked, "14:21", t0)
            .checkpoints
            .is_empty());
        let mut idle_exit = exit;
        idle_exit.released_mid_turn = false;
        idle_exit.key = Some("claude-s1".into());
        assert!(tracker
            .on_edge("w1:p1", idle_exit, "14:22", t0)
            .checkpoints
            .is_empty());
    }

    #[test]
    fn milestone_titles_count_the_commits() {
        let one = milestone("k".into(), &["abc\tship it".into()]).unwrap();
        assert_eq!(one.title, "commit: ship it");
        let many: Vec<String> = (0..=MAX_COMMITS).map(|i| format!("c{i}\ts{i}")).collect();
        let many = milestone("k".into(), &many).unwrap();
        assert_eq!(many.title, format!("{MAX_COMMITS}+ commits: s0"));
        assert_eq!(many.detail.unwrap().lines().count(), MAX_COMMITS);
        assert_eq!(milestone("k".into(), &[]), None);
    }

    #[test]
    fn git_log_lines_before_the_turn_are_not_its_commits() {
        let text = "abc\t2000\tnew one\ndef\t500\told one\nbad line\n";
        assert_eq!(parse_log(text, 1000), ["abc\tnew one"]);
    }

    #[test]
    fn head_is_read_from_the_repository_files() {
        let root = std::env::temp_dir().join(format!(
            "herdr-auto-head-{}-{}",
            std::process::id(),
            crate::coordinator::launch::new_uuid()
        ));
        let git = root.join(".git");
        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        // packed only
        std::fs::write(
            git.join("packed-refs"),
            format!("# pack-refs with: peeled\n{SHA_A} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(read_head(&root.join("src")).as_deref(), Some(SHA_A));
        // a loose ref wins
        std::fs::write(git.join("refs/heads/main"), format!("{SHA_B}\n")).unwrap();
        assert_eq!(read_head(&root).as_deref(), Some(SHA_B));
        // detached
        std::fs::write(git.join("HEAD"), format!("{SHA_A}\n")).unwrap();
        assert_eq!(read_head(&root).as_deref(), Some(SHA_A));
        // a linked worktree: `.git` is a file, refs live in the common dir
        let linked = root.join("wt");
        let wt_git = git.join("worktrees/wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", wt_git.display()),
        )
        .unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        std::fs::write(wt_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(read_head(&linked).as_deref(), Some(SHA_B));
        // unborn branch
        std::fs::write(git.join("HEAD"), "ref: refs/heads/none\n").unwrap();
        assert_eq!(read_head(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tracker_stays_bounded() {
        let mut tracker = AutoTracker::default();
        let now = Instant::now();
        for i in 0..(MAX_TRACKED + 10) {
            let _ = tracker.on_edge(
                &format!("w1:p{i}"),
                facts(EdgeStatus::Idle, EdgeStatus::Working, TurnEdge::None),
                "14:00",
                now,
            );
        }
        assert_eq!(tracker.len(), MAX_TRACKED);
    }
}
