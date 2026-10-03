//! The notes worker (fork): one background thread for the transcript work
//! the app thread must not do (directory scans, large reads).
//!
//! Jobs go through a bounded queue; a full queue is reported to the caller,
//! which answers `pending` and lets the client ask again. Results come back
//! through a sink (the app posts them as `AppEvent::NotesWorkerFinished`).

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use super::transcript::{self, Anchor, Flavor};
use crate::agent_resume::PersistedAgentSession;
use crate::api::schema::notes::{CheckpointContextInfo, CheckpointContextSource};

/// Jobs waiting for the worker.
pub(crate) const QUEUE_CAPACITY: usize = 32;
/// Default and largest characters per side of a context.
pub(crate) const DEFAULT_CONTEXT_CHARS: u32 = 600;
pub(crate) const MAX_CONTEXT_CHARS: u32 = 4000;

/// How to find a session's live transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Locate {
    /// A Claude session (the native transcript under `~/.claude/projects`).
    Claude(PersistedAgentSession),
    /// A Codex session: the rollout `-<id>.jsonl` under `<codex home>/sessions`.
    Codex {
        codex_home: PathBuf,
        session_id: String,
    },
}

impl Locate {
    pub(crate) fn flavor(&self) -> Flavor {
        match self {
            Self::Claude(_) => Flavor::Claude,
            Self::Codex { .. } => Flavor::Codex,
        }
    }

    /// The memo key of the session (`<agent>:<session value>`).
    pub(crate) fn memo_key(&self) -> String {
        match self {
            Self::Claude(session) => format!("claude:{}", session.session_ref.value),
            Self::Codex { session_id, .. } => format!("codex:{session_id}"),
        }
    }

    fn find(&self) -> Option<PathBuf> {
        match self {
            Self::Claude(session) => {
                crate::agent_resume::native_transcript_locations(session).map(|found| found.file)
            }
            Self::Codex {
                codex_home,
                session_id,
            } => transcript::find_codex_rollout(codex_home, session_id),
        }
    }
}

/// One unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotesJob {
    /// Find a session's live transcript; with `anchor_for`, capture the
    /// anchor at its current end for that `(key, checkpoint id)`.
    Locate {
        locate: Locate,
        anchor_for: Option<(String, String)>,
    },
    /// Read the prompt and reply around a checkpoint's anchor.
    Context(ContextJob),
    /// Read a pane repo's HEAD (and, at a turn end, the turn's commits) for
    /// the automatic checkpoints.
    GitHead(super::auto::GitProbe),
}

/// A `checkpoints.context` read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextJob {
    pub key: String,
    pub id: String,
    pub chars: u32,
    pub flavor: Flavor,
    pub anchor: Anchor,
    /// The session's live transcript as known now (tried after the
    /// anchor's own path).
    pub native: Option<PathBuf>,
    /// How to find the live transcript when `native` is unknown.
    pub locate: Option<Locate>,
    /// herdr's backup directory of the session, when known.
    pub backup_dir: Option<PathBuf>,
}

/// What a job produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotesWorkerResult {
    Located {
        memo_key: String,
        path: Option<PathBuf>,
        anchor_for: Option<(String, String)>,
        anchor: Option<Anchor>,
    },
    Context {
        key: String,
        id: String,
        chars: u32,
        info: CheckpointContextInfo,
        /// A live transcript found on the way, for the memo.
        located: Option<(String, Option<PathBuf>)>,
    },
    GitHead {
        probe: super::auto::GitProbe,
        head: Option<String>,
        /// `<short sha>\t<subject>`, newest first (turn ends only).
        commits: Vec<String>,
    },
}

/// The anchor at the current end of `path`: the transcript reader's capture
/// (≤ 64 KiB tail read), else the path and its length.
pub(crate) fn anchor_at_end(path: &Path, flavor: Flavor) -> Option<Anchor> {
    if let Some(anchor) = transcript::capture_anchor(path, flavor) {
        return Some(Anchor {
            path: anchor.path.or_else(|| Some(path.to_path_buf())),
            ..anchor
        });
    }
    let metadata = std::fs::metadata(path).ok()?;
    metadata.is_file().then(|| Anchor {
        path: Some(path.to_path_buf()),
        offset: Some(metadata.len()),
        uuid: None,
    })
}

/// Run one job (on the worker thread; directly in tests).
pub(crate) fn run_job(job: NotesJob) -> NotesWorkerResult {
    match job {
        NotesJob::Locate { locate, anchor_for } => {
            let path = locate.find();
            let anchor = match (&anchor_for, &path) {
                (Some(_), Some(path)) => anchor_at_end(path, locate.flavor()),
                _ => None,
            };
            NotesWorkerResult::Located {
                memo_key: locate.memo_key(),
                path,
                anchor_for,
                anchor,
            }
        }
        NotesJob::Context(job) => run_context(job),
        NotesJob::GitHead(probe) => {
            let head = super::auto::read_head(&probe.cwd);
            let commits = match (&probe.since, &head) {
                (Some(since), Some(head)) if since != head => {
                    super::auto::commits_between(&probe.cwd, since, head, probe.turn_started_unix)
                }
                _ => Vec::new(),
            };
            NotesWorkerResult::GitHead {
                probe,
                head,
                commits,
            }
        }
    }
}

fn run_context(job: ContextJob) -> NotesWorkerResult {
    let mut located = None;
    let mut candidates: Vec<(PathBuf, CheckpointContextSource)> = Vec::new();
    if let Some(path) = job.anchor.path.clone() {
        candidates.push((path, CheckpointContextSource::Native));
    }
    let native = match (&job.native, &job.locate) {
        (Some(path), _) => Some(path.clone()),
        (None, Some(locate)) => {
            let found = locate.find();
            located = Some((locate.memo_key(), found.clone()));
            found
        }
        (None, None) => None,
    };
    if let Some(path) = native {
        if !candidates.iter().any(|(candidate, _)| *candidate == path) {
            candidates.push((path, CheckpointContextSource::Native));
        }
    }
    if let Some(dir) = &job.backup_dir {
        candidates.push((
            dir.join("transcript.jsonl"),
            CheckpointContextSource::Backup,
        ));
        candidates.push((
            dir.join("transcript.prev.jsonl"),
            CheckpointContextSource::BackupPrevious,
        ));
    }
    let chars = job.chars.clamp(1, MAX_CONTEXT_CHARS);
    let mut info = CheckpointContextInfo {
        id: job.id.clone(),
        source: CheckpointContextSource::Missing,
        prompt: None,
        reply: None,
        prompt_ts: None,
        reply_ts: None,
        continued: false,
        truncated: false,
    };
    for (path, source) in candidates {
        if !is_file(&path) {
            continue;
        }
        match transcript::context_at(&path, &job.anchor, job.flavor, chars) {
            Ok(extract) => {
                info = CheckpointContextInfo {
                    id: job.id.clone(),
                    source,
                    prompt: extract.prompt,
                    reply: extract.reply,
                    prompt_ts: extract.prompt_ts,
                    reply_ts: extract.reply_ts,
                    continued: extract.continued,
                    truncated: extract.truncated,
                };
                break;
            }
            Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    err = %err,
                    "notes: cannot read checkpoint context"
                );
            }
        }
    }
    NotesWorkerResult::Context {
        key: job.key,
        id: job.id,
        chars: job.chars,
        info,
        located,
    }
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Where results go.
pub(crate) type ResultSink = Box<dyn Fn(NotesWorkerResult) + Send>;

/// The worker thread's queue.
pub(crate) struct NotesWorker {
    tx: mpsc::SyncSender<NotesJob>,
}

impl std::fmt::Debug for NotesWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotesWorker").finish_non_exhaustive()
    }
}

impl NotesWorker {
    /// Start the thread. It ends when the worker (its sender) is dropped.
    pub(crate) fn spawn(sink: ResultSink) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<NotesJob>(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("herdr-notes-worker".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    sink(run_job(job));
                }
            })?;
        Ok(Self { tx })
    }

    /// Queue a job; `false` when the queue is full or the thread stopped.
    pub(crate) fn submit(&self, job: NotesJob) -> bool {
        self.tx.try_send(job).is_ok()
    }
}
