//! The file work of agent handoffs (fork), on one `herdr-agent-handoff`
//! worker thread so the server loop never waits on a disk or on git: the
//! transcript copy, the `latest.txt` pointer, `meta.json`, the handoff
//! file check and herdr's mechanical handoff. Jobs run in order; results go
//! back through a channel the server tick drains.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{Kind, Mechanical, RunPaths};

/// The transcript tail read for the last prompts.
const PROMPTS_TAIL_BYTES: u64 = 1024 * 1024;
/// How long `git status` may take for a mechanical handoff (a hung git, a
/// network mount): after it the handoff is written without git lines.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) enum Job {
    /// Create the run's directory, write `meta.json`, copy the transcript.
    /// `latest.txt` moves only once the run has a handoff ([`Job::Check`],
    /// [`Job::Mechanical`]).
    Start {
        run: u64,
        paths: RunPaths,
        /// The transcript path the agent's hooks reported.
        source: Option<PathBuf>,
        /// Else: the session to look the transcript up for, under `home`.
        locate: Option<crate::agent_context::ContextSession>,
        home: Option<PathBuf>,
        meta: serde_json::Value,
    },
    /// Copy the transcript again (right before the compaction, so the copy
    /// has the handoff turn too); answers [`Outcome::Started`].
    Copy {
        run: u64,
        paths: RunPaths,
        source: Option<PathBuf>,
        locate: Option<crate::agent_context::ContextSession>,
        home: Option<PathBuf>,
    },
    /// Whether the handoff file exists and is not empty (the turn that
    /// ended at `end`); a written one moves `latest.txt` to the run.
    Check {
        run: u64,
        end: Instant,
        paths: RunPaths,
    },
    /// Read git and the last prompts, write herdr's own handoff. A handoff
    /// the agent already wrote there is kept (`Written` answers
    /// [`Kind::Agent`]).
    Mechanical {
        run: u64,
        paths: RunPaths,
        input: Box<Mechanical>,
    },
    /// Rewrite `meta.json` (the run's outcome).
    Meta {
        paths: RunPaths,
        meta: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Started {
        run: u64,
        /// The transcript copy: its bytes, or why there is none.
        transcript: Result<u64, String>,
    },
    Checked {
        run: u64,
        end: Instant,
        nonempty: bool,
    },
    /// The run's handoff is there: who wrote it, or why herdr could not.
    Written {
        run: u64,
        result: Result<Kind, String>,
    },
}

/// The worker: jobs in, outcomes out. Dropping it ends the thread once its
/// queue is empty.
pub(crate) struct Worker {
    jobs: mpsc::Sender<Job>,
    outcomes: mpsc::Receiver<Outcome>,
}

impl Worker {
    pub(crate) fn spawn() -> std::io::Result<Self> {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (outcome_tx, outcomes) = mpsc::channel::<Outcome>();
        crate::thread_spawn::spawn_named("herdr-agent-handoff", move || {
            while let Ok(job) = job_rx.recv() {
                if let Some(outcome) = run_job(job) {
                    if outcome_tx.send(outcome).is_err() {
                        break;
                    }
                }
            }
        })?;
        Ok(Self { jobs, outcomes })
    }

    /// Queue a job; `false` when the thread is gone.
    pub(crate) fn submit(&self, job: Job) -> bool {
        self.jobs.send(job).is_ok()
    }

    /// The outcomes finished since the last call.
    pub(crate) fn drain(&self) -> Vec<Outcome> {
        self.outcomes.try_iter().collect()
    }
}

/// Run one job (on the worker; directly in tests).
pub(crate) fn run_job(job: Job) -> Option<Outcome> {
    match job {
        Job::Start {
            run,
            paths,
            source,
            locate,
            home,
            meta,
        } => {
            let source = resolve_source(source, locate.as_ref(), home.as_deref());
            Some(Outcome::Started {
                run,
                transcript: start(&paths, source.as_deref(), &meta),
            })
        }
        Job::Copy {
            run,
            paths,
            source,
            locate,
            home,
        } => {
            let source = resolve_source(source, locate.as_ref(), home.as_deref());
            Some(Outcome::Started {
                run,
                transcript: copy_transcript(&paths, source.as_deref()),
            })
        }
        Job::Check { run, end, paths } => {
            let nonempty = nonempty(&paths.handoff);
            if nonempty {
                write_latest(&paths);
            }
            Some(Outcome::Checked { run, end, nonempty })
        }
        Job::Mechanical { run, paths, input } => {
            let result = if nonempty(&paths.handoff) {
                Ok(Kind::Agent)
            } else {
                write_mechanical(&paths, *input)
                    .map(|()| Kind::Mechanical)
                    .map_err(|err| err.to_string())
            };
            if result.is_ok() {
                write_latest(&paths);
            }
            Some(Outcome::Written { run, result })
        }
        Job::Meta { paths, meta } => {
            if let Err(err) = write_json(&paths.meta, &meta) {
                tracing::warn!(err = %err, dir = %paths.dir.display(), "agent handoff: cannot write meta.json");
            }
            None
        }
    }
}

/// The reported transcript when it is there, else the session's file
/// looked up under `home`.
fn resolve_source(
    source: Option<PathBuf>,
    locate: Option<&crate::agent_context::ContextSession>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    source.filter(|path| path.is_file()).or_else(|| {
        locate.and_then(|session| crate::agent_context::locate_session_file(session, home, None))
    })
}

fn start(paths: &RunPaths, source: Option<&Path>, meta: &serde_json::Value) -> Result<u64, String> {
    std::fs::create_dir_all(&paths.dir)
        .map_err(|err| format!("create {}: {err}", paths.dir.display()))?;
    if let Err(err) = write_json(&paths.meta, meta) {
        tracing::warn!(err = %err, "agent handoff: cannot write meta.json");
    }
    copy_transcript(paths, source)
}

fn copy_transcript(paths: &RunPaths, source: Option<&Path>) -> Result<u64, String> {
    let source = source.ok_or_else(|| "the transcript path is not known".to_string())?;
    copy_atomic(source, &paths.transcript)
        .map_err(|err| format!("copy {}: {err}", source.display()))
}

/// Point `<session>/latest.txt` at the run (it has a handoff now).
fn write_latest(paths: &RunPaths) {
    if let Err(err) = write_atomic(
        &paths.latest,
        format!("{}\n", paths.dir.display()).as_bytes(),
    ) {
        tracing::warn!(err = %err, "agent handoff: cannot write latest.txt");
    }
}

/// Whether `path` is a file with content.
pub(crate) fn nonempty(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{}", std::process::id()));
    path.with_file_name(name)
}

/// Write `bytes` to `path` through a temp file and a rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

fn write_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

fn copy_atomic(source: &Path, dest: &Path) -> std::io::Result<u64> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = temp_path(dest);
    let copied = std::fs::copy(source, &temp)?;
    std::fs::rename(&temp, dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })?;
    Ok(copied)
}

/// The last [`PROMPTS_TAIL_BYTES`] of `path` as text (from a line start).
fn read_tail(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(PROMPTS_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(PROMPTS_TAIL_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Ok(text);
    }
    // The first line is cut.
    Ok(text
        .split_once('\n')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_default())
}

/// `git status --porcelain=v1 -b` in `cwd` (branch line first), bounded.
fn git_status(cwd: &Path) -> Vec<String> {
    git_status_with(Path::new("git"), cwd, GIT_TIMEOUT)
}

/// [`git_status`] with the program and the time limit (tests).
pub(crate) fn git_status_with(program: &Path, cwd: &Path, timeout: Duration) -> Vec<String> {
    let mut command = crate::noninteractive_process::command(program);
    command
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain=v1", "-b", "--untracked-files=normal"]);
    let Some(stdout) = bounded_stdout(command, timeout) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&stdout);
    let mut lines: Vec<String> = text
        .lines()
        .take(super::MECHANICAL_GIT_LINES)
        .map(str::to_string)
        .collect();
    let total = text.lines().count();
    if total > lines.len() {
        lines.push(format!("… {} more", total - lines.len()));
    }
    lines
}

/// Run `command` and return its stdout when it exits successfully within
/// `timeout`; else kill it and return `None`. stdout is read on a helper
/// thread so a full pipe cannot stall the child. On a timeout the helper is
/// not joined (a grandchild holding the pipe would hold the worker too); it
/// ends once the pipe closes.
fn bounded_stdout(mut command: std::process::Command, timeout: Duration) -> Option<Vec<u8>> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let reader = crate::thread_spawn::spawn_named("herdr-handoff-git", move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => break None,
        }
    };
    let Some(status) = status else {
        tracing::info!(
            timeout = ?timeout,
            "agent handoff: git did not answer in time; the handoff goes on without it"
        );
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    if !status.success() {
        return None;
    }
    // The child exited: its stdout closes unless a grandchild still holds it.
    let left = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(left.max(Duration::from_millis(200))).ok()
}

fn write_mechanical(paths: &RunPaths, mut input: Mechanical) -> std::io::Result<()> {
    if let Some(cwd) = input.cwd.clone() {
        input.git = git_status(&cwd);
    }
    // The prompts from the copy when there is one (the live file keeps
    // growing), else the live transcript.
    let source = if paths.transcript.is_file() {
        Some(paths.transcript.clone())
    } else {
        input.transcript.clone()
    };
    if let Some(source) = source {
        if let Ok(text) = read_tail(&source) {
            input.prompts = super::last_user_prompts(&text, super::MECHANICAL_PROMPTS);
        }
    }
    if paths.transcript.is_file() {
        input.transcript = Some(paths.transcript.clone());
    }
    let text = super::render_mechanical(&input);
    write_atomic(&paths.handoff, text.as_bytes())
}
