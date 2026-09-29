//! The AI news desk (fork): `news.run`, `news.status`, the schedule and the
//! run watcher.
//!
//! The server owns the news run. It installs the bundled runner into
//! `<home>/bin/` (`<home>` is `<session data dir>/news`), keeps one `News`
//! tab in the first space, and types
//! `python3 <home>/bin/news_run.py --home <home> --trigger <manual|scheduled>`
//! into that tab's shell, so the run is visible and interruptible like any
//! agent pane. The runner reports itself to the pane as the `news` agent
//! (source `herdr:news`), publishes an edition, appends a line to
//! `<home>/runs/index.jsonl` and execs the page viewer.
//!
//! Scheduling runs in the headless loop next to the tab-bar status tasks:
//! every `news.interval_hours`, deferred to the end of `news.quiet_hours`,
//! missed slots collapsed to one run, and only while `news.enabled`. A run
//! is in flight from its start until the run log gains a record started at
//! or after it (polled every five seconds) or the 65-minute watchdog fires
//! (Ctrl-C to the pane, a `timeout` record). A finished run that changed the
//! page marks the News tab important (the fork's `tab.set_reminder` state),
//! so it shows as unread.
//!
//! The client shell reads `news.get` (the pinned row, the settings section);
//! `news.open` focuses the News tab, creating it with the page viewer when
//! it is gone, or shows a past edition; `news.history` lists the editions;
//! `news.set_enabled` writes `news.enabled` to the config and reloads it.
//! Focusing the News tab clears its important mark here, on the scheduler
//! pass, so it works from any client.
//!
//! Everything the server must remember survives in `news.json` next to
//! `session.json` ([`crate::persist::news`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::{
    NewsGetInfo, NewsHistoryParams, NewsOpenParams, NewsRunInfo, NewsRunRecord,
    NewsSetEnabledParams, NewsStatusInfo, ResponseResult,
};
use crate::config::{NewsConfig, QuietHours};
use crate::persist::news as store;

/// The News tab's label.
pub(crate) const NEWS_TAB_LABEL: &str = "News";
/// The source and agent label the runner reports with (`pane.report_agent`):
/// a plain hook source with a label no screen manifest owns, so herdr applies
/// its state without seeing a process (a `claude` label would stay with
/// screen detection, which never sees the `claude -p` child).
pub(crate) const NEWS_HOOK_SOURCE: &str = "herdr:news";
pub(crate) const NEWS_AGENT_LABEL: &str = "news";
/// How often the run log is polled while a run is in flight.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// A run still in flight after this long is recorded `timeout` and
/// interrupted (the runner's own hang watchdog is 60 minutes).
const WATCHDOG: Duration = Duration::from_secs(65 * 60);
/// How often a starting run re-probes the pane for its shell prompt.
const START_RETRY: Duration = Duration::from_millis(500);
/// How long a starting run waits for the shell prompt (after `q` to the
/// viewer) before it is recorded `failed`.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Ctrl-C.
const INTERRUPT: &[u8] = b"\x03";
/// Quits the page viewer.
const VIEWER_QUIT: &[u8] = b"q";
/// The page viewer asset, under the home's `bin/`.
const VIEWER: &str = "viewer.py";

/// A command waiting for the News pane's shell prompt (`news.open`: the
/// viewer for a new tab or a past edition). Not persisted: a restart drops
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingPaneCommand {
    pub(crate) command: String,
    pub(crate) next_check: Instant,
    pub(crate) give_up_at: Instant,
    /// Whether `q` was already sent to whatever holds the pane (a fresh
    /// tab's shell gets none).
    pub(crate) quit_sent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewsTrigger {
    Manual,
    Scheduled,
}

impl NewsTrigger {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "scheduled" {
            Self::Scheduled
        } else {
            Self::Manual
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewsPhase {
    /// The command is not typed yet: the pane was busy (the viewer, or a
    /// stray process) and got `q`; the shell prompt is probed until
    /// `give_up_at`.
    Starting {
        next_check: Instant,
        give_up_at: Instant,
    },
    /// The command was typed; the run log is polled.
    Running { next_poll: Instant },
}

impl NewsPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Starting { .. } => "starting",
            Self::Running { .. } => "running",
        }
    }
}

/// A run in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewsRun {
    /// Seconds since the Unix epoch.
    pub(crate) started_at: u64,
    /// The same instant as the runner writes it (`YYYY-MM-DDTHH:MM:SS+00:00`).
    pub(crate) started: String,
    pub(crate) trigger: NewsTrigger,
    /// Byte length of the run log when the run started: only later lines
    /// can be its record.
    pub(crate) index_len: u64,
    /// When the watchdog started (the start, or the server boot for a run
    /// restored from `news.json`).
    pub(crate) watch_from: Instant,
    pub(crate) phase: NewsPhase,
}

impl NewsRun {
    fn info(&self) -> NewsRunInfo {
        NewsRunInfo {
            started_at: self.started_at,
            trigger: self.trigger.name().into(),
            phase: self.phase.name().into(),
        }
    }

    fn persisted(&self) -> store::PersistedNewsRun {
        store::PersistedNewsRun {
            started_at: self.started_at,
            started: self.started.clone(),
            trigger: self.trigger.name().into(),
            index_len: self.index_len,
        }
    }
}

/// Why a run did not start (or `news.open` did not open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NewsStartError {
    /// No news home: the session is not persisted.
    Unavailable,
    /// A run is already in flight.
    InFlight,
    /// Installing the runner or creating the tab failed.
    Failed(String),
    /// `news.open --edition`: no such edition.
    NoEdition(u32),
    /// `news.set_enabled`: the config file could not be written.
    ConfigWrite(String),
}

impl NewsStartError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "news_unavailable",
            Self::InFlight => "news_run_in_flight",
            Self::Failed(_) => "news_run_failed",
            Self::NoEdition(_) => "news_edition_not_found",
            Self::ConfigWrite(_) => "news_config_write_failed",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Unavailable => {
                "the news desk needs a persisted session (no session data directory)".into()
            }
            Self::InFlight => "a news run is already in flight".into(),
            Self::Failed(message) | Self::ConfigWrite(message) => message.clone(),
            Self::NoEdition(edition) => format!("edition {edition} does not exist"),
        }
    }
}

/// What the scheduler does on a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScheduleAction {
    Wait,
    Run,
    /// Quiet hours: the run waits until this time (seconds since the epoch).
    Defer(u64),
}

/// The local wall clock, as far as the schedule needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalClock {
    pub(crate) minute_of_day: u16,
    pub(crate) second: u8,
}

impl LocalClock {
    fn now() -> Option<Self> {
        let local = crate::platform::local_datetime()?;
        Some(Self {
            minute_of_day: u16::from(local.hour()) * 60 + u16::from(local.minute()),
            second: local.second(),
        })
    }
}

/// Whether a due run starts now, waits, or is deferred past quiet hours.
/// A never-scheduled run (`next_run_at` unset) is due at once; a slot in
/// the past by any amount is one run, not several. Without a local clock
/// quiet hours cannot be evaluated and the run starts.
pub(crate) fn schedule_action(
    enabled: bool,
    next_run_at: Option<u64>,
    run_in_flight: bool,
    now: u64,
    local: Option<LocalClock>,
    quiet: Option<QuietHours>,
) -> ScheduleAction {
    if !enabled || run_in_flight {
        return ScheduleAction::Wait;
    }
    if now < next_run_at.unwrap_or(now) {
        return ScheduleAction::Wait;
    }
    if let (Some(local), Some(quiet)) = (local, quiet) {
        if quiet.contains(local.minute_of_day) {
            let wait = u64::from(quiet.minutes_until_end(local.minute_of_day)) * 60;
            return ScheduleAction::Defer(now + wait.saturating_sub(u64::from(local.second)));
        }
    }
    ScheduleAction::Run
}

/// The record of the run started at `started` among records appended to
/// the run log after it began: the first one started at or after it. Both
/// timestamps are `YYYY-MM-DDTHH:MM:SS` UTC (plus an offset suffix), so
/// the first 19 characters compare as text.
pub(crate) fn completed_record<'a>(
    records: &'a [NewsRunRecord],
    started: &str,
) -> Option<&'a NewsRunRecord> {
    let key = |text: &str| text.get(..19).map(str::to_string);
    let ours = key(started)?;
    records
        .iter()
        .find(|record| key(&record.started).is_some_and(|theirs| theirs >= ours))
}

/// `now` as the runner writes it: ISO 8601 UTC with a `+00:00` suffix.
pub(crate) fn iso_utc(unix: u64) -> String {
    let date = time::OffsetDateTime::from_unix_timestamp(i64::try_from(unix).unwrap_or(0))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        date.year(),
        u8::from(date.month()),
        date.day(),
        date.hour(),
        date.minute(),
        date.second()
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// `text` as one POSIX shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The command typed into the News pane.
pub(crate) fn run_command(home: &Path, trigger: NewsTrigger, model: Option<&str>) -> String {
    let home = home.display().to_string();
    let mut command = format!(
        "python3 {} --home {} --trigger {}",
        shell_quote(
            &crate::integration::news_assets::runner_path(Path::new(&home))
                .display()
                .to_string()
        ),
        shell_quote(&home),
        trigger.name()
    );
    if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
        command.push_str(" --model ");
        command.push_str(&shell_quote(model));
    }
    command
}

/// The page viewer command (`news.open`): the latest page, or `edition`.
pub(crate) fn viewer_command(home: &Path, edition: Option<u32>) -> String {
    let viewer = home
        .join(crate::integration::news_assets::BIN_DIR)
        .join(VIEWER);
    let mut command = format!(
        "python3 {} {}",
        shell_quote(&viewer.display().to_string()),
        shell_quote(&store::page_path(home).display().to_string())
    );
    if let Some(edition) = edition {
        command.push_str(&format!(" --edition {edition}"));
    }
    command
}

/// Server-side news state (`App.news`).
#[derive(Debug)]
pub(crate) struct NewsState {
    pub(crate) enabled: bool,
    pub(crate) interval: Duration,
    pub(crate) quiet: Option<QuietHours>,
    pub(crate) quiet_text: String,
    pub(crate) model: Option<String>,
    /// `<session data dir>/news`; `None` without a persisted session, and
    /// then `news.run` refuses.
    pub(crate) home: Option<PathBuf>,
    /// `news.json`; `None` keeps everything in memory (tests).
    pub(crate) store: Option<PathBuf>,
    pub(crate) next_run_at: Option<u64>,
    pub(crate) tab_id: Option<String>,
    pub(crate) pane_id: Option<String>,
    pub(crate) run: Option<NewsRun>,
    pub(crate) consecutive_failures: u32,
    /// A viewer command waiting for the pane's shell prompt (`news.open`).
    pub(crate) pending_command: Option<PendingPaneCommand>,
    /// Tests: treat the pane as at a shell prompt without probing its
    /// foreground process (test shells are `cat`, never a shell).
    #[cfg(test)]
    pub(crate) assume_shell_ready: bool,
    /// Tests: treat the pane as busy (never at a shell prompt).
    #[cfg(test)]
    pub(crate) assume_shell_busy: bool,
}

impl NewsState {
    /// State for a server: `home` and `store` from the session data
    /// directory when the session is persisted, the record loaded from the
    /// store (a run in flight is watched from `now`).
    pub(crate) fn new(config: &NewsConfig, persisted: bool, now: Instant) -> Self {
        let (home, store) = if persisted {
            (Some(store::home_dir()), Some(store::store_path()))
        } else {
            (None, None)
        };
        let mut state = Self::in_memory(config, home);
        state.store = store;
        if let Some(store_path) = state.store.as_deref() {
            state.load_record(store::load(store_path), now);
        }
        state
    }

    /// State with nothing on disk but `home` (tests).
    pub(crate) fn in_memory(config: &NewsConfig, home: Option<PathBuf>) -> Self {
        let mut state = Self {
            enabled: false,
            interval: Duration::from_secs(u64::from(config.effective_interval_hours()) * 3600),
            quiet: None,
            quiet_text: String::new(),
            model: None,
            home,
            store: None,
            next_run_at: None,
            tab_id: None,
            pane_id: None,
            run: None,
            consecutive_failures: 0,
            pending_command: None,
            #[cfg(test)]
            assume_shell_ready: false,
            #[cfg(test)]
            assume_shell_busy: false,
        };
        state.apply_config(config);
        state
    }

    pub(crate) fn apply_config(&mut self, config: &NewsConfig) {
        self.enabled = config.enabled;
        self.interval = Duration::from_secs(u64::from(config.effective_interval_hours()) * 3600);
        self.quiet = config.quiet_hours();
        self.quiet_text = config.quiet_hours.trim().to_string();
        self.model = config
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string);
    }

    fn load_record(&mut self, record: store::NewsRecord, now: Instant) {
        self.next_run_at = record.next_run_at;
        self.tab_id = record.tab_id;
        self.pane_id = record.pane_id;
        self.consecutive_failures = record.consecutive_failures;
        self.run = record.run.map(|run| NewsRun {
            started_at: run.started_at,
            started: run.started,
            trigger: NewsTrigger::parse(&run.trigger),
            index_len: run.index_len,
            watch_from: now,
            phase: NewsPhase::Running { next_poll: now },
        });
    }

    fn record(&self) -> store::NewsRecord {
        store::NewsRecord {
            next_run_at: self.next_run_at,
            tab_id: self.tab_id.clone(),
            pane_id: self.pane_id.clone(),
            run: self.run.as_ref().map(NewsRun::persisted),
            consecutive_failures: self.consecutive_failures,
        }
    }

    /// Write `news.json` (when there is one). Errors are logged, never
    /// propagated: the in-memory state is the truth for this server.
    pub(crate) fn persist(&self) {
        let Some(path) = self.store.as_deref() else {
            return;
        };
        if let Err(err) = store::save(path, &self.record()) {
            tracing::warn!(
                event = "news.persist",
                outcome = "error",
                path = %path.display(),
                err = %err,
                "failed to write the news record"
            );
        }
    }

    pub(crate) fn schedule_action(&self, now: u64, local: Option<LocalClock>) -> ScheduleAction {
        schedule_action(
            self.enabled,
            self.next_run_at,
            self.run.is_some(),
            now,
            local,
            self.quiet,
        )
    }

    /// The next instant the scheduler needs a tick.
    pub(crate) fn next_deadline(&self, now: Instant, now_unix: u64) -> Option<Instant> {
        let pending = self
            .pending_command
            .as_ref()
            .map(|pending| pending.next_check);
        if let Some(run) = &self.run {
            let watchdog = run.watch_from + WATCHDOG;
            return Some(match run.phase {
                NewsPhase::Starting { next_check, .. } => next_check.min(watchdog),
                NewsPhase::Running { next_poll } => next_poll.min(watchdog),
            });
        }
        if !self.enabled || self.home.is_none() {
            return pending;
        }
        let next = self.next_run_at.unwrap_or(now_unix);
        let scheduled = now + Duration::from_secs(next.saturating_sub(now_unix));
        Some(pending.map_or(scheduled, |pending| pending.min(scheduled)))
    }

    /// The last finished run, from the run log.
    fn last_run(&self) -> Option<NewsRunRecord> {
        self.home
            .as_deref()
            .and_then(|home| store::read_history(home, 1).into_iter().next())
    }

    pub(crate) fn status(&self) -> NewsStatusInfo {
        NewsStatusInfo {
            enabled: self.enabled,
            interval_hours: u32::try_from(self.interval.as_secs() / 3600).unwrap_or(u32::MAX),
            quiet_hours: self.quiet_text.clone(),
            model: self.model.clone(),
            home: self
                .home
                .as_ref()
                .map(|home| home.display().to_string())
                .unwrap_or_default(),
            next_run_at: (self.enabled && self.home.is_some())
                .then(|| self.next_run_at.unwrap_or_else(unix_now)),
            tab_id: self.tab_id.clone(),
            pane_id: self.pane_id.clone(),
            run: self.run.as_ref().map(NewsRun::info),
            consecutive_failures: self.consecutive_failures,
            recent: self
                .home
                .as_deref()
                .map(|home| store::read_history(home, store::MAX_HISTORY))
                .unwrap_or_default(),
        }
    }

    /// Account for a finished run: the failure counter and the schedule.
    fn finish(&mut self, record: &NewsRunRecord) {
        self.run = None;
        if record.succeeded() {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
    }
}

/// The News tab's pane, resolved.
struct NewsPane {
    ws_idx: usize,
    tab_idx: usize,
    pane_id: crate::layout::PaneId,
}

impl App {
    pub(super) fn handle_news_run(&mut self, id: String) -> String {
        match self.start_news_run(NewsTrigger::Manual, Instant::now()) {
            Ok(_) => encode_success(
                id,
                ResponseResult::NewsStatus {
                    status: self.news.status(),
                },
            ),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    pub(super) fn handle_news_status(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::NewsStatus {
                status: self.news.status(),
            },
        )
    }

    pub(super) fn handle_news_get(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::NewsGet {
                news: self.news_get_info(),
            },
        )
    }

    pub(super) fn handle_news_history(&mut self, id: String, params: NewsHistoryParams) -> String {
        let Some(home) = self.news.home.as_deref() else {
            let err = NewsStartError::Unavailable;
            return encode_error(id, err.code(), err.message());
        };
        let mut editions = store::read_editions(home);
        if let Some(days) = params.days.filter(|days| *days > 0) {
            // Whole local days, newest first: today counts as one.
            let mut kept_days = editions
                .iter()
                .map(|edition| edition.day.clone())
                .collect::<Vec<_>>();
            kept_days.sort();
            kept_days.dedup();
            let keep = kept_days
                .iter()
                .rev()
                .take(days as usize)
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            editions.retain(|edition| keep.contains(&edition.day));
        }
        encode_success(id, ResponseResult::NewsHistory { editions })
    }

    pub(super) fn handle_news_open(&mut self, id: String, params: NewsOpenParams) -> String {
        match self.open_news(params.edition, Instant::now()) {
            Ok(()) => encode_success(
                id,
                ResponseResult::NewsGet {
                    news: self.news_get_info(),
                },
            ),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    pub(super) fn handle_news_set_enabled(
        &mut self,
        id: String,
        params: NewsSetEnabledParams,
    ) -> String {
        match self.set_news_enabled(params.enabled) {
            Ok(()) => encode_success(
                id,
                ResponseResult::NewsGet {
                    news: self.news_get_info(),
                },
            ),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    /// `news.get`: the schedule, the tab (only while it exists), the run in
    /// flight, the last run and the unread mark.
    pub(crate) fn news_get_info(&self) -> NewsGetInfo {
        let pane = self.existing_news_pane();
        let unread = pane.as_ref().is_some_and(|pane| {
            self.state
                .workspaces
                .get(pane.ws_idx)
                .and_then(|ws| ws.tabs.get(pane.tab_idx))
                .is_some_and(|tab| tab.important)
        });
        NewsGetInfo {
            enabled: self.news.enabled,
            interval_hours: u32::try_from(self.news.interval.as_secs() / 3600).unwrap_or(u32::MAX),
            quiet_hours: self.news.quiet_text.clone(),
            model: self.news.model.clone(),
            tab_id: pane
                .as_ref()
                .and_then(|pane| self.public_tab_id(pane.ws_idx, pane.tab_idx)),
            pane_id: pane
                .as_ref()
                .and_then(|pane| self.public_pane_id(pane.ws_idx, pane.pane_id)),
            next_run_at: (self.news.enabled && self.news.home.is_some())
                .then(|| self.news.next_run_at.unwrap_or_else(unix_now)),
            run: self.news.run.as_ref().map(NewsRun::info),
            last_run: self.news.last_run().map(|record| record.last_run()),
            unread,
            consecutive_failures: self.news.consecutive_failures,
        }
    }

    /// `news.set_enabled`: write `news.enabled` to the config file and reload
    /// it, so the file stays the truth and the running server follows.
    pub(crate) fn set_news_enabled(&mut self, enabled: bool) -> Result<(), NewsStartError> {
        crate::config::write_edit(crate::config::ConfigEdit::NewsEnabled(enabled))
            .map_err(NewsStartError::ConfigWrite)?;
        let report = self.reload_config();
        if self.news.enabled != enabled {
            // The reload kept an invalid file's previous sections; the
            // written value still applies to this server.
            tracing::warn!(
                event = "news.set_enabled",
                outcome = "applied_directly",
                status = ?report.status,
                "config reload did not apply news.enabled; applying it in memory"
            );
            self.news.enabled = enabled;
        }
        tracing::info!(
            event = "news.set_enabled",
            outcome = "ok",
            enabled,
            "news scheduling changed"
        );
        self.news.persist();
        Ok(())
    }

    /// `news.open`: focus the News tab. A tab that had to be created gets
    /// the page viewer (when there is a page); with `edition`, whatever
    /// holds the pane is quit with `q` and the viewer opens that edition.
    /// Refused with `edition` while a run is in flight.
    pub(crate) fn open_news(
        &mut self,
        edition: Option<u32>,
        now: Instant,
    ) -> Result<(), NewsStartError> {
        if edition.is_some() && self.news.run.is_some() {
            return Err(NewsStartError::InFlight);
        }
        let home = self.news.home.clone().ok_or(NewsStartError::Unavailable)?;
        if let Some(edition) = edition {
            if !store::read_editions(&home)
                .iter()
                .any(|entry| entry.edition == edition)
            {
                return Err(NewsStartError::NoEdition(edition));
            }
        }
        let (pane, created) = self
            .ensure_news_tab(&home)
            .map_err(NewsStartError::Failed)?;
        self.state.switch_workspace_tab(pane.ws_idx, pane.tab_idx);
        self.schedule_session_save();
        let show_viewer = edition.is_some()
            || (created && self.news.run.is_none() && store::page_path(&home).is_file());
        if show_viewer {
            std::fs::create_dir_all(&home)
                .and_then(|()| crate::integration::news_assets::install(&home))
                .map_err(|err| {
                    NewsStartError::Failed(format!(
                        "failed to install the news viewer under {}: {err}",
                        home.display()
                    ))
                })?;
            self.news.pending_command = Some(PendingPaneCommand {
                command: viewer_command(&home, edition),
                next_check: now,
                give_up_at: now + START_TIMEOUT,
                quit_sent: created,
            });
            self.drive_news_pending_command(now);
        }
        tracing::info!(
            event = "news.open",
            outcome = "ok",
            edition = edition.unwrap_or(0),
            created,
            viewer = show_viewer,
            "News tab focused"
        );
        Ok(())
    }

    /// The pending viewer command: type it once the pane is at a shell
    /// prompt, else send `q` once and probe again; give up after the start
    /// timeout. Returns whether anything was sent.
    fn drive_news_pending_command(&mut self, now: Instant) -> bool {
        let Some(pending) = self.news.pending_command.clone() else {
            return false;
        };
        if now < pending.next_check {
            return false;
        }
        let Some(pane) = self.existing_news_pane() else {
            self.news.pending_command = None;
            return false;
        };
        #[allow(unused_mut)] // the test override below assigns it
        let mut ready = self
            .lookup_runtime_sender(pane.ws_idx, pane.pane_id)
            .is_some_and(|runtime| super::agents::available_shell_name(runtime).is_some());
        #[cfg(test)]
        {
            ready = (ready || self.news.assume_shell_ready) && !self.news.assume_shell_busy;
        }
        if ready {
            self.news.pending_command = None;
            let mut command = pending.command;
            command.push('\r');
            if let Err(err) = self.news_pane_bytes(&pane, Bytes::from(command)) {
                tracing::warn!(
                    event = "news.open",
                    outcome = "command_failed",
                    err = %err,
                    "could not type the viewer command"
                );
            }
            return true;
        }
        if now >= pending.give_up_at {
            tracing::warn!(
                event = "news.open",
                outcome = "no_prompt",
                "the News pane never returned to a shell prompt; viewer not started"
            );
            self.news.pending_command = None;
            return false;
        }
        let mut sent = false;
        if !pending.quit_sent {
            sent = self
                .news_pane_bytes(&pane, Bytes::from_static(VIEWER_QUIT))
                .is_ok();
        }
        self.news.pending_command = Some(PendingPaneCommand {
            next_check: now + START_RETRY,
            quit_sent: true,
            ..pending
        });
        sent
    }

    /// The unread mark clears when the News tab is looked at: its
    /// `important` flag goes once it is the focused tab of the focused
    /// space. Returns whether it changed.
    fn clear_news_unread_when_focused(&mut self) -> bool {
        let Some(pane) = self.existing_news_pane() else {
            return false;
        };
        if self.state.active != Some(pane.ws_idx) {
            return false;
        }
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(pane.ws_idx)
            .filter(|ws| ws.active_tab == pane.tab_idx)
            .and_then(|ws| ws.tabs.get_mut(pane.tab_idx))
        else {
            return false;
        };
        if !tab.important {
            return false;
        }
        tab.important = false;
        tracing::info!(
            event = "news.unread",
            outcome = "cleared",
            "News tab focused; unread mark cleared"
        );
        self.state.mark_session_dirty();
        self.schedule_session_save();
        true
    }

    /// The scheduler's contribution to the headless loop deadline.
    pub(crate) fn next_news_deadline(&self, now: Instant) -> Option<Instant> {
        self.news.next_deadline(now, unix_now())
    }

    /// One scheduler pass: clear the unread mark of a focused News tab,
    /// drive the run in flight (launch, poll, watchdog) or the pending
    /// viewer command, or start a due scheduled run. Returns whether shared
    /// state changed.
    pub(crate) fn handle_news_tasks(&mut self, now: Instant) -> bool {
        let unread_cleared = self.clear_news_unread_when_focused();
        if self.news.run.is_some() {
            self.news.pending_command = None;
            return self.drive_news_run(now) || unread_cleared;
        }
        self.drive_news_pending_command(now);
        match self.news.schedule_action(unix_now(), LocalClock::now()) {
            ScheduleAction::Wait => unread_cleared,
            ScheduleAction::Defer(until) => {
                if self.news.next_run_at != Some(until) {
                    tracing::info!(
                        event = "news.schedule",
                        outcome = "deferred",
                        until,
                        "news run deferred past quiet hours"
                    );
                    self.news.next_run_at = Some(until);
                    self.news.persist();
                }
                unread_cleared
            }
            ScheduleAction::Run => match self.start_news_run(NewsTrigger::Scheduled, now) {
                Ok(_) => true,
                Err(err) => {
                    tracing::warn!(
                        event = "news.schedule",
                        outcome = "start_failed",
                        code = err.code(),
                        err = %err.message(),
                        "scheduled news run did not start"
                    );
                    // Try again next interval rather than every tick.
                    self.news.next_run_at = Some(unix_now() + self.news.interval.as_secs());
                    self.news.persist();
                    unread_cleared
                }
            },
        }
    }

    /// Start a run: install the runner, ensure the News tab, type the
    /// command (or `q` first when the pane is busy), record the run.
    pub(crate) fn start_news_run(
        &mut self,
        trigger: NewsTrigger,
        now: Instant,
    ) -> Result<NewsRunInfo, NewsStartError> {
        if self.news.run.is_some() {
            return Err(NewsStartError::InFlight);
        }
        let home = self.news.home.clone().ok_or(NewsStartError::Unavailable)?;
        std::fs::create_dir_all(&home)
            .and_then(|()| crate::integration::news_assets::install(&home))
            .map_err(|err| {
                NewsStartError::Failed(format!(
                    "failed to install the news runner under {}: {err}",
                    home.display()
                ))
            })?;
        let (pane, _) = self
            .ensure_news_tab(&home)
            .map_err(NewsStartError::Failed)?;
        // A run takes the pane: a viewer waiting for the prompt is dropped.
        self.news.pending_command = None;
        let started_at = unix_now();
        self.news.run = Some(NewsRun {
            started_at,
            started: iso_utc(started_at),
            trigger,
            index_len: store::index_len(&home),
            watch_from: now,
            phase: NewsPhase::Starting {
                next_check: now,
                give_up_at: now + START_TIMEOUT,
            },
        });
        self.news.next_run_at = Some(started_at + self.news.interval.as_secs());
        tracing::info!(
            event = "news.run",
            outcome = "started",
            trigger = trigger.name(),
            tab_id = self.news.tab_id.as_deref().unwrap_or(""),
            "news run started"
        );
        self.launch_news_run(&pane, now, true);
        self.news.persist();
        Ok(self
            .news
            .run
            .as_ref()
            .map(NewsRun::info)
            .unwrap_or(NewsRunInfo {
                started_at,
                trigger: trigger.name().into(),
                phase: "failed".into(),
            }))
    }

    /// The News tab: the stored one when it still exists and is still
    /// labelled `News`, else a new tab in the first space (not focused).
    /// The flag says whether it was created.
    fn ensure_news_tab(&mut self, home: &Path) -> Result<(NewsPane, bool), String> {
        if let Some(pane) = self.existing_news_pane() {
            return Ok((pane, false));
        }
        let Some(ws_idx) = (!self.state.workspaces.is_empty()).then_some(0) else {
            return Err("no space to open the News tab in".into());
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let shell_mode = self.state.shell_mode;
        let (tab_idx, terminal, runtime) = self.state.workspaces[ws_idx]
            .create_tab(
                rows,
                cols,
                home.to_path_buf(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                crate::pane::PaneShellConfig::new(&default_shell, shell_mode),
                Vec::new(),
            )
            .map_err(|err| format!("failed to open the News tab: {err}"))?;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        let tab = &mut self.state.workspaces[ws_idx].tabs[tab_idx];
        tab.set_custom_name(NEWS_TAB_LABEL.into());
        let pane_id = tab.root_pane;
        self.state.remove_alias_shadowed_by_new_pane(pane_id);
        self.news.tab_id = self.public_tab_id(ws_idx, tab_idx);
        self.news.pane_id = self.public_pane_id(ws_idx, pane_id);
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_tab_created_events(ws_idx, tab_idx);
        tracing::info!(
            event = "news.tab",
            outcome = "created",
            tab_id = self.news.tab_id.as_deref().unwrap_or(""),
            "News tab created"
        );
        Ok((
            NewsPane {
                ws_idx,
                tab_idx,
                pane_id,
            },
            true,
        ))
    }

    fn existing_news_pane(&self) -> Option<NewsPane> {
        let (ws_idx, tab_idx) = self.parse_tab_id(self.news.tab_id.as_deref()?)?;
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        if tab.custom_name.as_deref() != Some(NEWS_TAB_LABEL) {
            return None;
        }
        Some(NewsPane {
            ws_idx,
            tab_idx,
            pane_id: tab.root_pane,
        })
    }

    fn news_pane_bytes(&self, pane: &NewsPane, bytes: Bytes) -> Result<(), String> {
        let runtime = self
            .lookup_runtime_sender(pane.ws_idx, pane.pane_id)
            .ok_or_else(|| "the News pane has no shell".to_string())?;
        runtime.try_send_bytes(bytes).map_err(|err| err.to_string())
    }

    /// Type the command when the pane is at a shell prompt; otherwise send
    /// `q` (once, on the first attempt) and keep the run in `Starting`.
    fn launch_news_run(&mut self, pane: &NewsPane, now: Instant, first: bool) {
        #[allow(unused_mut)] // the test override below assigns it
        let mut ready = self
            .lookup_runtime_sender(pane.ws_idx, pane.pane_id)
            .is_some_and(|runtime| super::agents::available_shell_name(runtime).is_some());
        #[cfg(test)]
        {
            ready = (ready || self.news.assume_shell_ready) && !self.news.assume_shell_busy;
        }
        let (trigger, give_up_at) = match self.news.run.as_ref() {
            Some(run) => match run.phase {
                NewsPhase::Starting { give_up_at, .. } => (run.trigger, give_up_at),
                NewsPhase::Running { .. } => return,
            },
            None => return,
        };
        if ready {
            self.clear_stale_news_identity(pane);
            let home = self.news.home.clone().unwrap_or_default();
            let mut command = run_command(&home, trigger, self.news.model.as_deref());
            command.push('\r');
            match self.news_pane_bytes(pane, Bytes::from(command)) {
                Ok(()) => {
                    if let Some(run) = self.news.run.as_mut() {
                        run.phase = NewsPhase::Running {
                            next_poll: now + POLL_INTERVAL,
                        };
                    }
                }
                Err(err) => self.fail_news_run(format!("could not type the command: {err}")),
            }
            return;
        }
        if now >= give_up_at {
            self.fail_news_run("the News pane never returned to a shell prompt".into());
            return;
        }
        if first {
            if let Err(err) = self.news_pane_bytes(pane, Bytes::from_static(VIEWER_QUIT)) {
                self.fail_news_run(format!("could not reach the News pane: {err}"));
                return;
            }
        }
        if let Some(run) = self.news.run.as_mut() {
            run.phase = NewsPhase::Starting {
                next_check: now + START_RETRY,
                give_up_at,
            };
        }
    }

    /// The News pane is at a shell prompt, so whatever agent identity it
    /// still carries is over: a session or hook authority claimed by an
    /// interactive `claude` run there earlier (a different owner makes the
    /// server drop the runner's reports), and the recent-exit marker that
    /// makes it drop plain hook reports for claude after a claude process
    /// left the pane. The runner reports as `herdr:news` and needs a clean
    /// pane.
    fn clear_stale_news_identity(&mut self, pane: &NewsPane) {
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(pane.ws_idx)
            .and_then(|ws| ws.tabs.get(pane.tab_idx))
            .and_then(|tab| tab.terminal_id(pane.pane_id))
            .cloned()
        else {
            return;
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return;
        };
        tracing::info!(
            event = "news.run",
            outcome = "identity_cleared",
            terminal = %terminal_id,
            "clearing the News pane's stale agent identity before the run"
        );
        terminal.clear_agent_runtime_identity_after_respawn();
        self.state.mark_session_dirty();
        self.schedule_session_save();
    }

    /// The run in flight: launch when starting, poll the run log when
    /// running, time out past the watchdog.
    fn drive_news_run(&mut self, now: Instant) -> bool {
        let Some(run) = self.news.run.clone() else {
            return false;
        };
        if now >= run.watch_from + WATCHDOG {
            self.time_out_news_run();
            return true;
        }
        let pane = self.existing_news_pane();
        match run.phase {
            NewsPhase::Starting { next_check, .. } => {
                if now < next_check {
                    return false;
                }
                match pane {
                    Some(pane) => self.launch_news_run(&pane, now, false),
                    None => self.fail_news_run("the News tab is gone".into()),
                }
                self.news.persist();
                true
            }
            NewsPhase::Running { next_poll } => {
                if now < next_poll {
                    return false;
                }
                let Some(home) = self.news.home.clone() else {
                    return false;
                };
                let records = match store::read_index_after(&home, run.index_len) {
                    Ok(records) => records,
                    Err(err) => {
                        tracing::warn!(
                            event = "news.watch",
                            outcome = "read_error",
                            err = %err,
                            "failed to read the news run log"
                        );
                        Vec::new()
                    }
                };
                if let Some(record) = completed_record(&records, &run.started).cloned() {
                    self.complete_news_run(record);
                    return true;
                }
                if let Some(run) = self.news.run.as_mut() {
                    run.phase = NewsPhase::Running {
                        next_poll: now + POLL_INTERVAL,
                    };
                }
                false
            }
        }
    }

    /// A run that never got going: recorded `failed` in the run log.
    fn fail_news_run(&mut self, reason: String) {
        let Some(run) = self.news.run.clone() else {
            return;
        };
        tracing::warn!(
            event = "news.run",
            outcome = "failed",
            trigger = run.trigger.name(),
            reason = %reason,
            "news run did not start"
        );
        let record = self.server_news_record(&run, "failed", reason);
        self.complete_news_run(record);
    }

    /// The watchdog: Ctrl-C to the pane, the agent released, a `timeout`
    /// record.
    fn time_out_news_run(&mut self) {
        let Some(run) = self.news.run.clone() else {
            return;
        };
        tracing::warn!(
            event = "news.run",
            outcome = "timeout",
            trigger = run.trigger.name(),
            "news run exceeded the watchdog; interrupting it"
        );
        if let Some(pane) = self.existing_news_pane() {
            if let Err(err) = self.news_pane_bytes(&pane, Bytes::from_static(INTERRUPT)) {
                tracing::warn!(
                    event = "news.run",
                    outcome = "interrupt_failed",
                    err = %err,
                    "could not interrupt the news run"
                );
            }
            // The runner cannot release its agent report after Ctrl-C.
            self.handle_internal_event(crate::events::AppEvent::HookAgentReleased {
                pane_id: pane.pane_id,
                source: NEWS_HOOK_SOURCE.into(),
                agent_label: NEWS_AGENT_LABEL.into(),
                known_agent: None,
                seq: None,
            });
        }
        let record = self.server_news_record(
            &run,
            "timeout",
            format!("no result after {} minutes", WATCHDOG.as_secs() / 60),
        );
        self.complete_news_run(record);
    }

    fn server_news_record(&self, run: &NewsRun, outcome: &str, error: String) -> NewsRunRecord {
        let ended_at = unix_now();
        let record = NewsRunRecord {
            started: run.started.clone(),
            ended: Some(iso_utc(ended_at)),
            trigger: run.trigger.name().into(),
            outcome: outcome.into(),
            seconds: Some(ended_at.saturating_sub(run.started_at)),
            errors: vec![error],
            ..NewsRunRecord::default()
        };
        if let Some(home) = self.news.home.as_deref() {
            if let Err(err) = store::append_index_record(home, &record) {
                tracing::warn!(
                    event = "news.run",
                    outcome = "record_failed",
                    err = %err,
                    "could not append the run record"
                );
            }
        }
        record
    }

    /// Account for the finished run and mark the tab when the page changed.
    fn complete_news_run(&mut self, record: NewsRunRecord) {
        tracing::info!(
            event = "news.run",
            outcome = %record.outcome,
            edition = record.edition.unwrap_or(0),
            cost_usd = record.cost_usd,
            turns = record.turns,
            changed = record.changed,
            "news run finished"
        );
        self.news.finish(&record);
        if record.outcome == "ok" && record.changed {
            self.mark_news_tab_important();
        }
        self.news.persist();
    }

    fn mark_news_tab_important(&mut self) {
        let Some(pane) = self.existing_news_pane() else {
            return;
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(pane.ws_idx)
            .and_then(|ws| ws.tabs.get_mut(pane.tab_idx))
        else {
            return;
        };
        if !tab.important {
            tab.important = true;
            self.state.mark_session_dirty();
            self.schedule_session_save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn quiet(text: &str) -> Option<QuietHours> {
        NewsConfig {
            quiet_hours: text.into(),
            ..NewsConfig::default()
        }
        .quiet_hours()
    }

    fn clock(hour: u16, minute: u16, second: u8) -> Option<LocalClock> {
        Some(LocalClock {
            minute_of_day: hour * 60 + minute,
            second,
        })
    }

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn schedule_runs_when_due_and_waits_before() {
        let night = quiet("00:00-08:00");
        let noon = clock(12, 0, 0);
        assert_eq!(
            schedule_action(true, Some(NOW + 1), false, NOW, noon, night),
            ScheduleAction::Wait
        );
        assert_eq!(
            schedule_action(true, Some(NOW), false, NOW, noon, night),
            ScheduleAction::Run
        );
        assert_eq!(
            schedule_action(true, None, false, NOW, noon, night),
            ScheduleAction::Run,
            "a never-scheduled desk runs at once"
        );
        assert_eq!(
            schedule_action(true, Some(NOW - 5 * 86_400), false, NOW, noon, night),
            ScheduleAction::Run,
            "missed slots collapse to one run"
        );
        assert_eq!(
            schedule_action(true, Some(NOW), false, NOW, None, night),
            ScheduleAction::Run,
            "no local clock: quiet hours cannot be evaluated"
        );
    }

    #[test]
    fn schedule_is_inert_while_disabled_or_in_flight() {
        let noon = clock(12, 0, 0);
        assert_eq!(
            schedule_action(false, Some(NOW - 10), false, NOW, noon, None),
            ScheduleAction::Wait
        );
        assert_eq!(
            schedule_action(false, None, false, NOW, noon, None),
            ScheduleAction::Wait
        );
        assert_eq!(
            schedule_action(true, Some(NOW - 10), true, NOW, noon, None),
            ScheduleAction::Wait
        );
    }

    #[test]
    fn quiet_hours_defer_a_due_run_to_their_end() {
        let night = quiet("00:00-08:00");
        assert_eq!(
            schedule_action(true, Some(NOW - 60), false, NOW, clock(3, 30, 15), night),
            ScheduleAction::Defer(NOW + (4 * 60 + 30) * 60 - 15)
        );
        assert_eq!(
            schedule_action(true, Some(NOW), false, NOW, clock(7, 59, 59), night),
            ScheduleAction::Defer(NOW + 1)
        );
        assert_eq!(
            schedule_action(true, Some(NOW), false, NOW, clock(8, 0, 0), night),
            ScheduleAction::Run
        );
        let wrapped = quiet("22:00-06:00");
        assert_eq!(
            schedule_action(true, Some(NOW), false, NOW, clock(23, 0, 0), wrapped),
            ScheduleAction::Defer(NOW + 7 * 3600)
        );
        assert_eq!(
            schedule_action(true, Some(NOW), false, NOW, clock(3, 0, 0), None),
            ScheduleAction::Run,
            "no quiet hours"
        );
    }

    fn record(started: &str, outcome: &str) -> NewsRunRecord {
        NewsRunRecord {
            started: started.into(),
            trigger: "manual".into(),
            outcome: outcome.into(),
            ..NewsRunRecord::default()
        }
    }

    #[test]
    fn the_completed_record_is_the_first_started_at_or_after_ours() {
        let ours = iso_utc(1_790_000_000);
        assert_eq!(ours, "2026-09-21T14:13:20+00:00");
        let records = [
            record("2026-09-21T14:13:19+00:00", "ok"),
            record("2026-09-21T14:13:20+00:00", "invalid"),
            record("2026-09-21T15:00:00+00:00", "ok"),
        ];
        assert_eq!(
            completed_record(&records, &ours).unwrap().outcome,
            "invalid"
        );
        assert!(completed_record(&records[..1], &ours).is_none());
        assert!(completed_record(&[], &ours).is_none());
        assert!(completed_record(&[record("soon", "ok")], &ours).is_none());
        let later = record("2026-09-21T14:13:21Z", "ok");
        assert!(completed_record(std::slice::from_ref(&later), &ours).is_some());
    }

    #[test]
    fn the_command_quotes_the_home_and_adds_the_model() {
        let home = Path::new("/tmp/it's news");
        assert_eq!(
            run_command(home, NewsTrigger::Manual, None),
            "python3 '/tmp/it'\\''s news/bin/news_run.py' --home '/tmp/it'\\''s news' --trigger manual"
        );
        assert_eq!(
            run_command(Path::new("/n"), NewsTrigger::Scheduled, Some(" opus ")),
            "python3 '/n/bin/news_run.py' --home '/n' --trigger scheduled --model 'opus'"
        );
        assert_eq!(
            run_command(Path::new("/n"), NewsTrigger::Scheduled, Some("  ")),
            "python3 '/n/bin/news_run.py' --home '/n' --trigger scheduled"
        );
    }

    fn temp_home(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-news-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn news_app(home: Option<PathBuf>, enabled: bool) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = Config::default();
        config.news.enabled = enabled;
        let mut app = App::new(
            &config,
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("bucket")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app.news = NewsState::in_memory(&config.news, home);
        app
    }

    fn request(app: &mut App, method: crate::api::schema::Method) -> serde_json::Value {
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method,
        });
        serde_json::from_str(&response).unwrap()
    }

    fn in_flight(started_at: u64, now: Instant) -> NewsRun {
        NewsRun {
            started_at,
            started: iso_utc(started_at),
            trigger: NewsTrigger::Manual,
            index_len: 0,
            watch_from: now,
            phase: NewsPhase::Running { next_poll: now },
        }
    }

    #[test]
    fn news_run_refuses_without_a_home_and_while_in_flight() {
        let mut app = news_app(None, false);
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsRun(Default::default()),
        );
        assert_eq!(response["error"]["code"], "news_unavailable");

        let home = temp_home("inflight");
        let mut app = news_app(Some(home.clone()), true);
        let now = Instant::now();
        app.news.run = Some(in_flight(NOW, now));
        assert_eq!(
            app.start_news_run(NewsTrigger::Scheduled, now),
            Err(NewsStartError::InFlight)
        );
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsRun(Default::default()),
        );
        assert_eq!(response["error"]["code"], "news_run_in_flight");
        assert_eq!(
            app.news.run.as_ref().unwrap().started_at,
            NOW,
            "the run is untouched"
        );
        assert!(
            !app.handle_news_tasks(now - Duration::from_secs(1)),
            "the scheduler never starts a second run"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn status_reports_the_schedule_the_run_and_the_history() {
        let home = temp_home("status");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        std::fs::write(
            store::index_path(&home),
            "{\"started\":\"2026-09-21T09:00:00+00:00\",\"trigger\":\"scheduled\",\"outcome\":\"ok\",\"edition\":1,\"cost_usd\":0.5,\"turns\":9,\"decision\":{\"changed\":true}}\n",
        )
        .unwrap();
        let mut app = news_app(Some(home.clone()), true);
        app.news.next_run_at = Some(NOW);
        app.news.consecutive_failures = 1;
        app.news.run = Some(in_flight(NOW - 60, Instant::now()));
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsStatus(Default::default()),
        );
        let status = &response["result"]["status"];
        assert_eq!(response["result"]["type"], "news_status");
        assert_eq!(status["enabled"], true);
        assert_eq!(status["interval_hours"], 6);
        assert_eq!(status["quiet_hours"], "00:00-08:00");
        assert_eq!(status["home"], home.display().to_string());
        assert_eq!(status["next_run_at"], NOW);
        assert_eq!(status["run"]["phase"], "running");
        assert_eq!(status["run"]["trigger"], "manual");
        assert_eq!(status["consecutive_failures"], 1);
        assert_eq!(status["recent"][0]["edition"], 1);
        assert_eq!(status["recent"][0]["changed"], true);

        let disabled = news_app(Some(home.clone()), false);
        assert!(disabled.news.status().next_run_at.is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_watcher_completes_the_run_from_the_log_and_marks_the_tab() {
        let home = temp_home("watch");
        let mut app = news_app(Some(home.clone()), true);
        let now = Instant::now();
        // The News tab is the existing first tab.
        app.state.workspaces[0].tabs[0].set_custom_name(NEWS_TAB_LABEL.into());
        app.news.tab_id = app.public_tab_id(0, 0);
        app.news.run = Some(in_flight(NOW, now));
        assert!(!app.handle_news_tasks(now), "nothing in the log yet");
        assert!(app.news.run.is_some());

        std::fs::create_dir_all(home.join("runs")).unwrap();
        std::fs::write(
            store::index_path(&home),
            format!(
                "{{\"started\":\"{}\",\"trigger\":\"manual\",\"outcome\":\"ok\",\"edition\":2,\"decision\":{{\"changed\":true}}}}\n",
                iso_utc(NOW + 3)
            ),
        )
        .unwrap();
        assert!(app.handle_news_tasks(now + POLL_INTERVAL));
        assert!(app.news.run.is_none());
        assert_eq!(app.news.consecutive_failures, 0);
        assert!(
            app.state.workspaces[0].tabs[0].important,
            "a changed page marks the tab"
        );

        // A failed run counts, and an older record does not complete it.
        app.state.workspaces[0].tabs[0].important = false;
        let mut run = in_flight(NOW + 100, now);
        run.index_len = store::index_len(&home);
        app.news.run = Some(run);
        assert!(!app.handle_news_tasks(now + POLL_INTERVAL));
        assert!(app.news.run.is_some(), "the earlier record is not ours");
        store::append_index_record(&home, &record(&iso_utc(NOW + 101), "invalid")).unwrap();
        assert!(app.handle_news_tasks(now + 2 * POLL_INTERVAL));
        assert!(app.news.run.is_none());
        assert_eq!(app.news.consecutive_failures, 1);
        assert!(!app.state.workspaces[0].tabs[0].important);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_stale_session_on_the_news_pane_is_cleared_when_the_command_is_typed() {
        let home = temp_home("stale");
        let mut app = news_app(Some(home.clone()), false);
        app.news.assume_shell_ready = true;
        app.state.workspaces[0].tabs[0].set_custom_name(NEWS_TAB_LABEL.into());
        app.news.tab_id = app.public_tab_id(0, 0);
        let tab = &app.state.workspaces[0].tabs[0];
        let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("stale").unwrap(),
            transcript_path: None,
        });
        assert!(terminal.persistable_agent_session().is_some());

        let info = app
            .start_news_run(NewsTrigger::Manual, Instant::now())
            .unwrap();
        assert_eq!(info.phase, "running");
        assert!(
            app.state.terminals[&terminal_id]
                .persistable_agent_session()
                .is_none(),
            "the earlier owner no longer blocks the runner's reports"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_watchdog_records_a_timeout() {
        let home = temp_home("watchdog");
        let mut app = news_app(Some(home.clone()), true);
        let now = Instant::now();
        app.news.run = Some(in_flight(NOW, now));
        assert!(app.handle_news_tasks(now + WATCHDOG));
        assert!(app.news.run.is_none());
        assert_eq!(app.news.consecutive_failures, 1);
        let history = store::read_history(&home, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "timeout");
        assert_eq!(history[0].started, iso_utc(NOW));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn deadlines_follow_the_run_and_the_schedule() {
        let home = temp_home("deadline");
        let mut app = news_app(Some(home.clone()), true);
        let now = Instant::now();
        app.news.next_run_at = Some(NOW + 600);
        assert_eq!(
            app.news.next_deadline(now, NOW),
            Some(now + Duration::from_secs(600))
        );
        assert_eq!(
            app.news.next_deadline(now, NOW + 1_000),
            Some(now),
            "a slot in the past is due now"
        );
        app.news.run = Some(in_flight(NOW, now));
        assert_eq!(app.news.next_deadline(now, NOW), Some(now));
        app.news.run = None;
        app.news.enabled = false;
        assert_eq!(app.news.next_deadline(now, NOW), None);
        let disabled_home = news_app(None, true);
        assert_eq!(disabled_home.news.next_deadline(now, NOW), None);
        let _ = std::fs::remove_dir_all(&home);
    }

    fn news_tab_at(app: &mut App, idx: usize) {
        app.state.workspaces[0].tabs[idx].set_custom_name(NEWS_TAB_LABEL.into());
        app.news.tab_id = app.public_tab_id(0, idx);
    }

    fn editions_index(home: &Path, editions: &[u32]) {
        std::fs::create_dir_all(home.join("editions")).unwrap();
        let entries = editions
            .iter()
            .map(|n| {
                serde_json::json!({
                    "edition": n, "path": format!("2026-09-{n:02}/0900-e{n:04}.json"),
                    "at": format!("2026-09-{n:02}T06:00:00+00:00"), "day": format!("2026-09-{n:02}"),
                    "trigger": "scheduled", "stories": 20 + n, "changed": n % 2 == 1
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(
            store::editions_index_path(home),
            serde_json::json!({ "version": 1, "editions": entries }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn news_get_reports_the_tab_only_while_it_exists_and_its_unread_mark() {
        let home = temp_home("get");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        store::append_index_record(
            &home,
            &NewsRunRecord {
                started: iso_utc(NOW - 600),
                ended: Some(iso_utc(NOW - 300)),
                trigger: "scheduled".into(),
                outcome: "ok".into(),
                edition: Some(4),
                changed: true,
                ..NewsRunRecord::default()
            },
        )
        .unwrap();
        let mut app = news_app(Some(home.clone()), false);
        app.news.tab_id = Some("w_9:t_9".into());
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsGet(Default::default()),
        );
        assert_eq!(response["result"]["type"], "news_get", "{response}");
        let news = &response["result"]["news"];
        assert!(news.get("tab_id").is_none(), "a gone tab is not reported");
        assert_eq!(news["unread"], false);
        assert_eq!(news["enabled"], false);
        assert!(news.get("next_run_at").is_none());
        assert_eq!(news["last_run"]["edition"], 4);
        assert_eq!(news["last_run"]["started_at"], NOW - 600);
        assert_eq!(news["last_run"]["outcome"], "ok");

        news_tab_at(&mut app, 0);
        app.state.workspaces[0].tabs[0].important = true;
        let info = app.news_get_info();
        assert_eq!(info.tab_id, app.public_tab_id(0, 0));
        assert!(info.unread);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_unread_mark_clears_on_the_scheduler_pass_once_the_tab_is_focused() {
        let home = temp_home("unread");
        let mut app = news_app(Some(home.clone()), false);
        app.state.workspaces[0].test_add_tab(Some("other"));
        news_tab_at(&mut app, 1);
        app.state.workspaces[0].tabs[1].important = true;
        app.state.switch_workspace_tab(0, 0);
        let now = Instant::now();
        assert!(!app.handle_news_tasks(now), "unfocused: the mark stays");
        assert!(app.state.workspaces[0].tabs[1].important);

        app.state.switch_workspace_tab(0, 1);
        assert!(app.handle_news_tasks(now), "focused: the mark clears");
        assert!(!app.state.workspaces[0].tabs[1].important);
        assert!(!app.handle_news_tasks(now), "and nothing changes after");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn news_open_creates_the_tab_with_the_viewer_focuses_it_and_shows_editions() {
        let home = temp_home("open");
        let mut app = news_app(None, false);
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsOpen(Default::default()),
        );
        assert_eq!(response["error"]["code"], "news_unavailable");

        let mut app = news_app(Some(home.clone()), false);
        app.news.assume_shell_ready = true;
        app.state.default_shell = "/bin/cat".into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(store::page_path(&home), "{}").unwrap();
        editions_index(&home, &[1, 2]);

        let response = request(
            &mut app,
            crate::api::schema::Method::NewsOpen(Default::default()),
        );
        assert_eq!(response["result"]["type"], "news_get", "{response}");
        let workspace = &app.state.workspaces[0];
        assert_eq!(workspace.tabs.len(), 2);
        assert_eq!(workspace.tabs[1].custom_name.as_deref(), Some("News"));
        assert_eq!(workspace.active_tab, 1, "news.open focuses the tab");
        assert_eq!(
            response["result"]["news"]["tab_id"],
            app.public_tab_id(0, 1).unwrap()
        );
        let tab = &workspace.tabs[1];
        let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
        let runtime = app.terminal_runtimes.get(&terminal_id).unwrap();
        for _ in 0..80 {
            if runtime
                .snapshot_history()
                .is_some_and(|text| text.contains("viewer.py"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let history = runtime.snapshot_history().unwrap_or_default();
        assert!(
            history.contains("viewer.py") && history.contains("page.json"),
            "a created tab gets the viewer: {history}"
        );
        assert!(!history.contains("--edition"), "{history}");
        assert!(app.news.pending_command.is_none());

        // An existing tab is only focused (no second viewer).
        app.state.switch_workspace_tab(0, 0);
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsOpen(Default::default()),
        );
        assert_eq!(response["result"]["type"], "news_get");
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert!(app.news.pending_command.is_none());

        // A past edition: refused while a run is in flight, unknown numbers
        // refused, else the viewer command carries it.
        app.news.run = Some(in_flight(NOW, Instant::now()));
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsOpen(crate::api::schema::NewsOpenParams {
                edition: Some(1),
            }),
        );
        assert_eq!(response["error"]["code"], "news_run_in_flight");
        app.news.run = None;
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsOpen(crate::api::schema::NewsOpenParams {
                edition: Some(9),
            }),
        );
        assert_eq!(response["error"]["code"], "news_edition_not_found");
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsOpen(crate::api::schema::NewsOpenParams {
                edition: Some(2),
            }),
        );
        assert_eq!(response["result"]["type"], "news_get", "{response}");
        let runtime = app.terminal_runtimes.get(&terminal_id).unwrap();
        for _ in 0..80 {
            if runtime
                .snapshot_history()
                .is_some_and(|text| text.contains("--edition 2"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let history = runtime.snapshot_history().unwrap_or_default();
        assert!(history.contains("--edition 2"), "{history}");

        // The history, whole and by days.
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsHistory(Default::default()),
        );
        assert_eq!(response["result"]["type"], "news_history");
        assert_eq!(response["result"]["editions"].as_array().unwrap().len(), 2);
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsHistory(crate::api::schema::NewsHistoryParams {
                days: Some(1),
            }),
        );
        let editions = response["result"]["editions"].as_array().unwrap();
        assert_eq!(editions.len(), 1);
        assert_eq!(editions[0]["edition"], 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_pending_viewer_command_quits_the_pane_once_then_waits_for_the_prompt() {
        let home = temp_home("pending");
        let mut app = news_app(Some(home.clone()), false);
        news_tab_at(&mut app, 0);
        let tab = &app.state.workspaces[0].tabs[0];
        let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id, runtime);
        let now = Instant::now();
        app.news.assume_shell_busy = true;
        app.news.pending_command = Some(PendingPaneCommand {
            command: "echo hi".into(),
            next_check: now,
            give_up_at: now + START_TIMEOUT,
            quit_sent: false,
        });
        app.handle_news_tasks(now);
        assert_eq!(rx.try_recv().unwrap().as_ref(), b"q", "the viewer is quit");
        let pending = app.news.pending_command.clone().expect("still pending");
        assert!(pending.quit_sent);
        assert_eq!(pending.next_check, now + START_RETRY);
        assert_eq!(
            app.news.next_deadline(now, NOW),
            Some(now + START_RETRY),
            "the loop wakes for the probe"
        );
        app.handle_news_tasks(now + START_RETRY);
        assert!(rx.try_recv().is_err(), "q is sent once");
        // The prompt is back.
        app.news.assume_shell_busy = false;
        app.handle_news_tasks(now + 2 * START_RETRY);
        assert_eq!(rx.try_recv().unwrap().as_ref(), b"echo hi\r");
        assert!(app.news.pending_command.is_none());

        // Never a prompt: given up after the start timeout.
        app.news.assume_shell_busy = true;
        app.news.pending_command = Some(PendingPaneCommand {
            command: "echo hi".into(),
            next_check: now,
            give_up_at: now + START_TIMEOUT,
            quit_sent: true,
        });
        app.handle_news_tasks(now + START_TIMEOUT);
        assert!(app.news.pending_command.is_none());
        assert!(rx.try_recv().is_err());

        // A run in flight drops it.
        app.news.pending_command = Some(PendingPaneCommand {
            command: "echo hi".into(),
            next_check: now,
            give_up_at: now + START_TIMEOUT,
            quit_sent: true,
        });
        app.news.run = Some(in_flight(NOW, now));
        app.handle_news_tasks(now);
        assert!(app.news.pending_command.is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn set_enabled_writes_the_config_and_applies_it() {
        let dir = temp_home("set-enabled");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[news]\nenabled = false\ninterval_hours = 3\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = news_app(Some(dir.join("news")), false);
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsSetEnabled(crate::api::schema::NewsSetEnabledParams {
                enabled: true,
            }),
        );
        assert_eq!(response["result"]["type"], "news_get", "{response}");
        assert_eq!(response["result"]["news"]["enabled"], true);
        assert!(app.news.enabled);
        let written: crate::config::Config =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written.news.enabled);
        assert_eq!(written.news.interval_hours, 3, "other keys are kept");

        let response = request(
            &mut app,
            crate::api::schema::Method::NewsSetEnabled(crate::api::schema::NewsSetEnabledParams {
                enabled: false,
            }),
        );
        assert_eq!(response["result"]["news"]["enabled"], false);
        assert!(!app.news.enabled);
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_persisted_run_is_watched_again_after_a_restart() {
        let dir = temp_home("restart");
        let store_path = dir.join(store::FILE_NAME);
        store::save(
            &store_path,
            &store::NewsRecord {
                next_run_at: Some(NOW + 5),
                tab_id: Some("w_1:t_1".into()),
                pane_id: None,
                run: Some(store::PersistedNewsRun {
                    started_at: NOW,
                    started: iso_utc(NOW),
                    trigger: "scheduled".into(),
                    index_len: 7,
                }),
                consecutive_failures: 3,
            },
        )
        .unwrap();
        let now = Instant::now();
        let mut state = NewsState::in_memory(&NewsConfig::default(), Some(dir.clone()));
        state.store = Some(store_path);
        state.load_record(store::load(state.store.as_deref().unwrap()), now);
        let run = state.run.as_ref().unwrap();
        assert_eq!(run.trigger, NewsTrigger::Scheduled);
        assert_eq!(run.index_len, 7);
        assert_eq!(run.watch_from, now);
        assert_eq!(run.phase, NewsPhase::Running { next_poll: now });
        assert_eq!(state.consecutive_failures, 3);
        assert_eq!(state.tab_id.as_deref(), Some("w_1:t_1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
