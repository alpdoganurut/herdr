//! The file work of agent handoffs (fork), on one `herdr-agent-handoff`
//! worker thread so the server loop never waits on a disk or on git: the
//! transcript copy, the `latest.txt` pointer, `meta.json`, the handoff
//! file check and herdr's mechanical handoff. Jobs run in order; results go
//! back through a channel the server tick drains.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use super::{Mechanical, RunPaths};

/// The transcript tail read for the last prompts.
const PROMPTS_TAIL_BYTES: u64 = 1024 * 1024;

pub(crate) enum Job {
    /// Create the run's directory, copy the transcript, point `latest.txt`
    /// at the run, write `meta.json`.
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
    /// Whether the handoff file exists and is not empty (the turn that
    /// ended at `end`).
    Check {
        run: u64,
        end: Instant,
        path: PathBuf,
    },
    /// Read git and the last prompts, write herdr's own handoff.
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
    Written {
        run: u64,
        result: Result<(), String>,
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
            let source = source.filter(|path| path.is_file()).or_else(|| {
                locate.as_ref().and_then(|session| {
                    crate::agent_context::locate_session_file(session, home.as_deref(), None)
                })
            });
            Some(Outcome::Started {
                run,
                transcript: start(&paths, source.as_deref(), &meta),
            })
        }
        Job::Check { run, end, path } => Some(Outcome::Checked {
            run,
            end,
            nonempty: nonempty(&path),
        }),
        Job::Mechanical { run, paths, input } => Some(Outcome::Written {
            run,
            result: write_mechanical(&paths, *input).map_err(|err| err.to_string()),
        }),
        Job::Meta { paths, meta } => {
            if let Err(err) = write_json(&paths.meta, &meta) {
                tracing::warn!(err = %err, dir = %paths.dir.display(), "agent handoff: cannot write meta.json");
            }
            None
        }
    }
}

fn start(paths: &RunPaths, source: Option<&Path>, meta: &serde_json::Value) -> Result<u64, String> {
    std::fs::create_dir_all(&paths.dir)
        .map_err(|err| format!("create {}: {err}", paths.dir.display()))?;
    if let Err(err) = write_atomic(
        &paths.latest,
        format!("{}\n", paths.dir.display()).as_bytes(),
    ) {
        tracing::warn!(err = %err, "agent handoff: cannot write latest.txt");
    }
    if let Err(err) = write_json(&paths.meta, meta) {
        tracing::warn!(err = %err, "agent handoff: cannot write meta.json");
    }
    let source = source.ok_or_else(|| "the transcript path is not known".to_string())?;
    copy_atomic(source, &paths.transcript)
        .map_err(|err| format!("copy {}: {err}", source.display()))
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
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain=v1", "-b", "--untracked-files=normal"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
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
