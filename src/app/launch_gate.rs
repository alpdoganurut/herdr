//! Typed launches into a pane's shell (fork): when a launch command may be
//! typed, and what is typed.
//!
//! A just-spawned shell is the pane's foreground job while it still runs its
//! rc files (a fastfetch splash, a prompt theme). Input typed then waits in
//! the tty's canonical input queue, which macOS caps at about 1 KiB (bytes
//! past it are dropped), and rc programs may read or flush it. So herdr's own
//! launches into fresh panes wait until the shell's line editor reads
//! ([`ShellGate::LineEditor`]: the terminal left canonical mode), retrying
//! until a deadline: `agents.open_tab` parks its reply
//! ([`App::handle_deferred_agents_open_tab`]), a closed-session reopen
//! queues its resume line, the coordinator start retries on its own clock.
//!
//! Independently, a long command line is never typed:
//! [`App::short_launch_line`] writes it to a one-shot script in the
//! coordinator directory and types a short `. '<script>'` instead, which the
//! shell runs exactly as the typed line (same aliases, functions and hooks;
//! the agent's argv is unchanged).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::api::schema::agents_model::{error_code, AgentsOpenResult};
use crate::api::schema::{AgentStartParams, Request, ResponseResult};
use crate::terminal::TerminalId;

use super::agents::AgentStartError;
use super::agents_model::{LogLine, ModelCaller, ModelError, ModelResult};
use super::App;

/// How long a launch waits for a fresh pane's shell to read input.
pub(crate) const SHELL_READY_TIMEOUT: Duration = Duration::from_secs(20);
/// How often a waiting launch checks the shell again.
pub(crate) const SHELL_READY_RETRY: Duration = Duration::from_millis(100);
/// Longer command lines go through a launch script (a typed line stays well
/// under the ~1 KiB tty input queue and under 512 bytes).
pub(crate) const TYPED_LAUNCH_DIRECT_MAX: usize = 256;
/// Launch scripts left behind (never sourced) are pruned after this.
const LAUNCH_SCRIPT_MAX_AGE: Duration = Duration::from_secs(3600);

/// How ready a pane's shell must be before a launch is typed into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellGate {
    /// The bare shell is the foreground job (`agent.start`, as upstream).
    Foreground,
    /// ...and, for a line-editing shell, its line editor reads input
    /// (herdr's own launches into fresh panes).
    LineEditor,
}

/// The parked `agents.open_tab` replies and reopen resume lines.
#[derive(Debug, Default)]
pub(crate) struct PendingLaunches {
    pub(crate) open_tabs: Vec<PendingOpenTab>,
    pub(crate) typed: Vec<PendingTypedLaunch>,
}

/// An `agents.open_tab` whose new tab's shell was not reading yet: its tab,
/// meta and team membership exist; the agent start and the reply wait.
#[derive(Debug)]
pub(crate) struct PendingOpenTab {
    pub(crate) request_id: String,
    pub(crate) respond_to: Option<std::sync::mpsc::Sender<String>>,
    pub(crate) caller: ModelCaller,
    pub(crate) line: LogLine,
    pub(crate) capped: bool,
    pub(crate) opened: AgentsOpenResult,
    pub(crate) start: AgentStartParams,
    pub(crate) deadline: Instant,
    pub(crate) next_try: Instant,
}

/// A resume line for a fresh pane, typed once its shell reads input.
#[derive(Debug)]
pub(crate) struct PendingTypedLaunch {
    pub(crate) terminal_id: TerminalId,
    pub(crate) bytes: Vec<u8>,
    pub(crate) deadline: Instant,
}

/// What [`App::try_model_start`] found.
pub(crate) enum StartAttempt {
    /// The shell is not reading yet (or an rc program holds the pane).
    NotReady,
    Failed(ModelError),
}

/// The shell verb that runs a script file in the current shell, when the
/// shell reads the POSIX-quoted lines herdr types (`.` or fish's `source`).
fn source_verb(shell_name: &str) -> Option<&'static str> {
    let name = shell_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell_name)
        .trim_start_matches('-');
    match name {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" => Some("."),
        "fish" => Some("source"),
        _ => None,
    }
}

/// Write `command` as a one-shot script that removes itself first.
fn write_launch_script(dir: &Path, command: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    prune_launch_scripts(dir);
    let path = dir.join(format!("{}.sh", crate::coordinator::launch::new_uuid()));
    let quoted =
        crate::platform::interactive_shell_command(&[path.to_string_lossy().into_owned()], "sh")
            .ok_or_else(|| std::io::Error::other("cannot quote the launch script path"))?;
    let body = format!("command rm -f -- {quoted}\n{command}\n");
    crate::coordinator::write_atomically(&path, body.as_bytes())?;
    Ok(path)
}

fn prune_launch_scripts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "sh") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > LAUNCH_SCRIPT_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl App {
    /// Where launch scripts go: the coordinator directory's `launch/`.
    fn launch_script_dir(&self) -> PathBuf {
        self.agents_model
            .dir
            .as_ref()
            .unwrap_or(&self.coordinator.dir)
            .join("launch")
    }

    /// The line to type for `command` in `shell_name`: the command itself
    /// when short, else `. '<script>'` running it from a one-shot script
    /// (shells without a source verb, or a failed write, type it as is).
    pub(super) fn short_launch_line(&self, command: String, shell_name: &str) -> String {
        if command.len() <= TYPED_LAUNCH_DIRECT_MAX {
            return command;
        }
        let Some(verb) = source_verb(shell_name) else {
            tracing::warn!(
                event = "agent.launch",
                shell = shell_name,
                bytes = command.len(),
                "a long launch line is typed as is (the shell has no source verb herdr uses)"
            );
            return command;
        };
        match write_launch_script(&self.launch_script_dir(), &command) {
            Ok(path) => crate::platform::interactive_shell_command(
                &[verb.to_string(), path.to_string_lossy().into_owned()],
                shell_name,
            )
            .unwrap_or(command),
            Err(err) => {
                tracing::warn!(
                    event = "agent.launch",
                    %err,
                    bytes = command.len(),
                    "cannot write a launch script; the long launch line is typed as is"
                );
                command
            }
        }
    }

    /// Whether `shell_name`, the pane's foreground job, reads a command line:
    /// a line-editing shell has the terminal out of canonical mode. Unknown
    /// (Windows, a test runtime) counts as reading.
    pub(super) fn line_editor_reading(
        &self,
        runtime: &crate::terminal::TerminalRuntime,
        shell_name: &str,
    ) -> bool {
        #[cfg(test)]
        {
            if self.coordinator.assume_shell_starting {
                return false;
            }
            if self.coordinator.assume_shell_ready {
                return true;
            }
        }
        !crate::platform::pane_shell_has_line_editor(shell_name)
            || runtime.input_is_raw() != Some(false)
    }

    /// Whether a fresh pane's shell is ready to read a typed launch.
    fn typed_launch_ready(&self, terminal_id: &TerminalId) -> Option<bool> {
        let runtime = self.terminal_runtimes.get(terminal_id)?;
        let shell = super::agents::available_shell_name(runtime);
        #[cfg(test)]
        let shell = shell.or_else(|| {
            self.coordinator
                .assume_shell_ready
                .then(|| "sh".to_string())
        });
        Some(shell.is_some_and(|shell| self.line_editor_reading(runtime, &shell)))
    }

    /// Type a reopen's resume line into a fresh pane now when its shell
    /// reads, else once it does (or at the deadline, best effort: the line
    /// is short). `Ok(true)`: typed now.
    pub(super) fn type_launch_when_ready(
        &mut self,
        terminal_id: &TerminalId,
        bytes: Vec<u8>,
        now: Instant,
    ) -> Result<bool, String> {
        match self.typed_launch_ready(terminal_id) {
            None => Err("the new tab has no shell".to_string()),
            Some(true) => {
                self.send_typed_launch(terminal_id, bytes)?;
                Ok(true)
            }
            Some(false) => {
                self.agents_model.launches.typed.push(PendingTypedLaunch {
                    terminal_id: terminal_id.clone(),
                    bytes,
                    deadline: now + SHELL_READY_TIMEOUT,
                });
                Ok(false)
            }
        }
    }

    fn send_typed_launch(
        &mut self,
        terminal_id: &TerminalId,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        let runtime = self
            .terminal_runtimes
            .get(terminal_id)
            .ok_or_else(|| "the new tab has no shell".to_string())?;
        runtime
            .try_send_bytes(Bytes::from(bytes))
            .map_err(|err| err.to_string())?;
        // A scripted write, for the turn origin.
        self.note_input(
            terminal_id,
            crate::agents_model::InputSource::Programmatic(crate::agents_model::Programmatic::Api),
        );
        Ok(())
    }

    /// Start a model launch in its fresh pane, if its shell reads.
    pub(super) fn try_model_start(&mut self, start: &AgentStartParams) -> Result<(), StartAttempt> {
        match self.start_agent_gated(start.clone(), ShellGate::LineEditor) {
            Ok(_) => Ok(()),
            Err(AgentStartError::ShellNotReady(_) | AgentStartError::TargetBusy(_)) => {
                Err(StartAttempt::NotReady)
            }
            Err(err) => {
                let body = self.agent_start_error_body(err);
                Err(StartAttempt::Failed(ModelError::new(
                    &body.code,
                    body.message,
                )))
            }
        }
    }

    /// `agents.open_tab` through the server loop: replies now, or parks the
    /// reply until the new tab's shell reads and the agent is typed in.
    pub(crate) fn handle_deferred_agents_open_tab(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        let crate::api::schema::Method::AgentsOpenTab(params) = request.method else {
            return false;
        };
        let id = request.id;
        match self.begin_agents_open_tab(&id, &params) {
            Ok(Some(open)) => {
                let _ = respond_to.send(Self::model_reply(
                    id,
                    Ok(ResponseResult::AgentsOpenTab { open }),
                ));
            }
            Ok(None) => {
                if let Some(pending) = self.agents_model.launches.open_tabs.last_mut() {
                    pending.respond_to = Some(respond_to);
                }
            }
            Err(err) => {
                let _ = respond_to.send(Self::model_reply(id, Err(err)));
            }
        }
        true
    }

    /// Park an open whose start waits for the shell.
    pub(super) fn park_open_tab(&mut self, pending: PendingOpenTab) {
        self.agents_model.launches.open_tabs.push(pending);
    }

    /// One step of every parked launch. Returns whether anything changed.
    pub(crate) fn drive_pending_launches(&mut self, now: Instant) -> bool {
        let mut changed = false;
        let open_tabs = std::mem::take(&mut self.agents_model.launches.open_tabs);
        for mut pending in open_tabs {
            if now < pending.next_try {
                self.agents_model.launches.open_tabs.push(pending);
                continue;
            }
            let result = match self.try_model_start(&pending.start) {
                Ok(()) => Ok(pending.opened.clone()),
                Err(StartAttempt::NotReady) if now < pending.deadline => {
                    pending.next_try = now + SHELL_READY_RETRY;
                    self.agents_model.launches.open_tabs.push(pending);
                    continue;
                }
                Err(StartAttempt::NotReady) => Err(self.undo_open_tab(
                    &pending.opened,
                    ModelError::new(
                        error_code::FAILED,
                        format!(
                            "the new tab's shell did not become ready to read the launch within {}s",
                            SHELL_READY_TIMEOUT.as_secs()
                        ),
                    ),
                )),
                Err(StartAttempt::Failed(err)) => Err(self.undo_open_tab(&pending.opened, err)),
            };
            changed = true;
            self.finish_pending_open_tab(pending, result);
        }

        let typed = std::mem::take(&mut self.agents_model.launches.typed);
        for pending in typed {
            match self.typed_launch_ready(&pending.terminal_id) {
                None => {
                    tracing::warn!(event = "agent.launch", terminal = %pending.terminal_id, "the pane closed before its resume line was typed");
                }
                Some(false) if now < pending.deadline => {
                    self.agents_model.launches.typed.push(pending);
                }
                Some(ready) => {
                    if !ready {
                        tracing::warn!(event = "agent.launch", terminal = %pending.terminal_id, "the shell never became ready; typing the resume line anyway");
                    }
                    if let Err(err) = self.send_typed_launch(&pending.terminal_id, pending.bytes) {
                        tracing::warn!(event = "agent.launch", terminal = %pending.terminal_id, %err, "cannot type the resume line");
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    fn finish_pending_open_tab(
        &mut self,
        pending: PendingOpenTab,
        result: ModelResult<AgentsOpenResult>,
    ) {
        let PendingOpenTab {
            request_id,
            respond_to,
            caller,
            line,
            capped,
            ..
        } = pending;
        let result = self.settle_open_tab(&caller, line, capped, result);
        if let Some(respond_to) = respond_to {
            let _ = respond_to.send(Self::model_reply(
                request_id,
                result.map(|open| ResponseResult::AgentsOpenTab { open }),
            ));
        }
    }

    /// The loop deadline for parked launches.
    pub(crate) fn next_pending_launch_deadline(&self) -> Option<Instant> {
        let launches = &self.agents_model.launches;
        let open = launches.open_tabs.iter().map(|pending| pending.next_try);
        let typed = launches
            .typed
            .iter()
            .map(|_| Instant::now() + SHELL_READY_RETRY);
        open.chain(typed).min()
    }
}

#[cfg(test)]
mod tests;
