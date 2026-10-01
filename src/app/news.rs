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
//! at each local time in `news.times`, only while `news.enabled`. The
//! schedule remembers when the last run (any trigger) started: a slot is
//! owed while it is the latest one reached today and no run has started
//! since it, so a slot missed while the server was down runs once on return
//! when it was earlier today, several missed slots collapse to one run, and
//! a manual run leaves the schedule alone (`schedule_action`). Quiet hours
//! only hold notifications. A desk that has never run (no record in the run
//! log and no published edition) does not wait for the first slot: when the
//! server starts with news enabled, or a config reload (`news.set_enabled`
//! included) turns it on, the next scheduler pass starts one run at once
//! (`NewsState::first_run_pending`, consumed once; a failed start stands for
//! it like a slot's). A run
//! is in flight from its start until the run log gains a record started at
//! or after it (polled every five seconds). The runner gets one budget for
//! the whole run on its command line (`--deadline-min`, [`RUN_BUDGET_MIN`]);
//! the server's watchdog is that budget plus a margin ([`WATCHDOG`]): past
//! it the News pane's foreground process group gets SIGTERM (the runner
//! turns it into its interrupt path and writes `interrupted`), or, when no
//! process can be found, two Ctrl-C a second apart; the run then stays in
//! flight until the runner's record appears or the pane is back at its shell
//! prompt, when a `timeout` record is written. A finished run that changed
//! the page marks the News tab important (the fork's `tab.set_reminder`
//! state), so it shows as unread.
//!
//! The client shell reads `news.get` (the pinned row, the settings section);
//! `news.open` focuses the News tab, creating it with the page viewer when
//! it is gone, or shows a past edition; `news.history` lists the editions;
//! `news.set_enabled` writes `news.enabled` to the config and reloads it,
//! `news.set_times` does the same for `news.times`.
//! Focusing the News tab clears its important mark here, on the scheduler
//! pass, so it works from any client.
//!
//! Notifications: a finished run that changed the page and asked for one
//! (`decision.notify`) queues at most one notification, two per local day
//! ([`DAILY_NOTIFY_CAP`]), a high-urgency one past the cap once a day, and a
//! run during quiet hours waits for their end; the third failed run in a row
//! queues one "News runs failing" alert until a success. The queue lives in
//! `news.json`; the headless server delivers it
//! ([`crate::server::headless`]'s news_notify) while a client shell is
//! connected, else when one attaches.
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
    NewsSetEnabledParams, NewsSetTimesParams, NewsStatusInfo, ResponseResult,
};
use crate::config::{format_hhmm, normalize_times, NewsConfig, QuietHours};
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
/// Minutes the runner may spend on the whole run (anchors, the editor and
/// its fix-up call together): passed as `--deadline-min`.
pub(crate) const RUN_BUDGET_MIN: u64 = 60;
/// A run still in flight after the runner's budget plus this margin is
/// interrupted by the server and recorded `timeout`.
const WATCHDOG_MARGIN: Duration = Duration::from_secs(10 * 60);
/// The server's watchdog: the runner's budget plus [`WATCHDOG_MARGIN`].
const WATCHDOG: Duration = Duration::from_secs(RUN_BUDGET_MIN * 60 + WATCHDOG_MARGIN.as_secs());
/// The gap between the two Ctrl-C the watchdog falls back to when it finds
/// no process to signal (two in one write coalesce into one SIGINT; the
/// pinned runner needs two within three seconds).
const INTERRUPT_GAP: Duration = Duration::from_secs(1);
/// How often a starting run re-probes the pane for its shell prompt.
const START_RETRY: Duration = Duration::from_millis(500);
/// How long a starting run waits for the shell prompt (after `q` to the
/// viewer) before it is recorded `failed`.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Run-log polls in a row that must find the pane at its shell before a run
/// with no record counts as interrupted.
const INTERRUPTED_AFTER_POLLS: u8 = 2;
/// Ctrl-C.
const INTERRUPT: &[u8] = b"\x03";
/// Kill-line: typed before every command so half-typed input on the
/// pane's prompt line does not end up in front of it.
const KILL_LINE: &str = "\x15";
/// Quits the page viewer. The viewer runs pinned (`--pinned`): it ignores
/// `q`, Esc and Ctrl-C, and only this private sequence (CSI 9999 ~) ends it.
const VIEWER_QUIT: &[u8] = b"\x1b[9999~";
/// Run notifications per local day; a high-urgency one may pass it once.
pub(crate) const DAILY_NOTIFY_CAP: u8 = 2;
/// Failed runs in a row that raise the failure alert.
pub(crate) const FAILURE_ALERT_AFTER: u32 = 3;
pub(crate) const PENDING_RUN: &str = "run";
const PENDING_FAILURES: &str = "failures";
/// Notification text limits, as `notification.show` applies them.
const NOTIFY_TITLE_CHARS: usize = 80;
/// Run notifications are titled `News: <the editor's title>`.
const NEWS_TITLE_PREFIX: &str = "News: ";
const NOTIFY_BODY_CHARS: usize = 240;

/// The policy's answer for a run notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotifyDecision {
    Deliver,
    Drop(&'static str),
}

/// Whether a run notification goes out today, charging the ledger: the
/// counters start over on a new local `day`; the first
/// [`DAILY_NOTIFY_CAP`] are delivered, then a `high` one once more. Asked
/// at delivery (the server's news_notify), not when a run queues one, so a
/// queued notification replaced before it went out uses no slot.
pub(crate) fn notify_decision(
    ledger: &mut store::NewsNotifyRecord,
    day: &str,
    high: bool,
) -> NotifyDecision {
    if ledger.day != day {
        ledger.day = day.to_string();
        ledger.delivered = 0;
        ledger.high_extra_used = false;
    }
    if ledger.delivered < DAILY_NOTIFY_CAP {
        ledger.delivered = ledger.delivered.saturating_add(1);
        return NotifyDecision::Deliver;
    }
    if high && !ledger.high_extra_used {
        ledger.high_extra_used = true;
        ledger.delivered = ledger.delivered.saturating_add(1);
        return NotifyDecision::Deliver;
    }
    NotifyDecision::Drop(if high {
        "the day's high-urgency extra is used"
    } else {
        "the daily cap is reached"
    })
}

/// The local day, `YYYY-MM-DD` (empty without a local clock).
fn local_day_now() -> String {
    crate::platform::local_datetime()
        .map(|local| {
            format!(
                "{:04}-{:02}-{:02}",
                local.year(),
                u8::from(local.month()),
                local.day()
            )
        })
        .unwrap_or_default()
}
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
    /// The watchdog fired and the runner was signalled; the run log is
    /// still polled for the runner's own record, and the pane for its
    /// shell prompt. `second_interrupt` is the Ctrl-C fallback's second
    /// write, still to be sent.
    Stopping {
        next_poll: Instant,
        second_interrupt: Option<Instant>,
    },
}

impl NewsPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Starting { .. } => "starting",
            Self::Running { .. } => "running",
            Self::Stopping { .. } => "stopping",
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

/// Why a news request was refused: a run did not start, `news.open` did
/// not open, `news.set_enabled` or `news.set_times` could not write,
/// `news.set_times` got a time that is not `HH:MM`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NewsError {
    /// No news home: the session is not persisted.
    Unavailable,
    /// A run is already in flight.
    InFlight,
    /// Installing the runner or creating the tab failed.
    Failed(String),
    /// `news.open --edition`: no such edition.
    NoEdition(u32),
    /// `news.set_enabled`, `news.set_times`: the config file could not be
    /// written.
    ConfigWrite(String),
    /// `news.set_times`: an entry is not a time of day.
    InvalidTime(String),
    /// `news.set_quiet_hours`: not a `HH:MM-HH:MM` window.
    InvalidQuietHours(String),
}

impl NewsError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "news_unavailable",
            Self::InFlight => "news_run_in_flight",
            Self::Failed(_) => "news_run_failed",
            Self::NoEdition(_) => "news_edition_not_found",
            Self::ConfigWrite(_) => "news_config_write_failed",
            Self::InvalidTime(_) => "news_invalid_time",
            Self::InvalidQuietHours(_) => "news_invalid_quiet_hours",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Unavailable => {
                "the news desk needs a persisted session (no session data directory)".into()
            }
            Self::InFlight => "a news run is already in flight".into(),
            Self::Failed(message) | Self::ConfigWrite(message) => message.clone(),
            Self::InvalidTime(err) => format!("{err}; expected HH:MM"),
            Self::InvalidQuietHours(err) => {
                format!("{err}; expected HH:MM-HH:MM or an empty window")
            }
            Self::NoEdition(edition) => format!("edition {edition} does not exist"),
        }
    }
}

/// What the scheduler does on a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScheduleAction {
    Wait,
    Run,
}

/// The local wall clock, as far as the schedule needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalClock {
    pub(crate) minute_of_day: u16,
    pub(crate) second: u8,
}

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

impl LocalClock {
    fn now() -> Option<Self> {
        let local = crate::platform::local_datetime()?;
        Some(Self {
            minute_of_day: u16::from(local.hour()) * 60 + u16::from(local.minute()),
            second: local.second(),
        })
    }

    /// Seconds since local midnight.
    fn since_midnight(self) -> u64 {
        u64::from(self.minute_of_day) * 60 + u64::from(self.second)
    }

    /// Today's slot at `minute_of_day` (local), in seconds since the epoch,
    /// given `now` in the same seconds. (On the day of a DST change the
    /// slots before the change are off by the shift; the schedule is
    /// re-evaluated on every tick, so nothing is lost.)
    fn slot_today(self, now: u64, minute_of_day: u16) -> u64 {
        now.saturating_sub(self.since_midnight()) + u64::from(minute_of_day) * 60
    }
}

/// The slot the schedule owes now: the latest listed time already reached
/// today, unless a run (any trigger) started at or after it. Earlier slots
/// today are folded into it (several missed slots are one run); yesterday's
/// are never owed. `times` are minutes since local midnight, sorted.
pub(crate) fn due_slot(
    times: &[u16],
    now: u64,
    local: LocalClock,
    last_started_at: Option<u64>,
) -> Option<u64> {
    let latest = times
        .iter()
        .map(|minute| local.slot_today(now, *minute))
        .filter(|slot| *slot <= now)
        .max()?;
    last_started_at
        .is_none_or(|started| started < latest)
        .then_some(latest)
}

/// The first listed time after `now`: later today, else the first one
/// tomorrow. `None` without times.
pub(crate) fn upcoming_slot(times: &[u16], now: u64, local: LocalClock) -> Option<u64> {
    let first = *times.first()?;
    times
        .iter()
        .map(|minute| local.slot_today(now, *minute))
        .find(|slot| *slot > now)
        .or_else(|| Some(local.slot_today(now, first) + SECONDS_PER_DAY))
}

/// The next scheduled run as reported: the slot owed now (in the past:
/// "due now"), else the upcoming one.
pub(crate) fn next_run_at(
    times: &[u16],
    now: u64,
    local: LocalClock,
    last_started_at: Option<u64>,
) -> Option<u64> {
    due_slot(times, now, local, last_started_at).or_else(|| upcoming_slot(times, now, local))
}

/// Whether a scheduled run starts now. Nothing starts while scheduling is
/// off, a run is in flight, the list is empty or there is no local clock
/// (the slots cannot be placed).
pub(crate) fn schedule_action(
    enabled: bool,
    times: &[u16],
    run_in_flight: bool,
    now: u64,
    local: Option<LocalClock>,
    last_started_at: Option<u64>,
) -> ScheduleAction {
    if !enabled || run_in_flight {
        return ScheduleAction::Wait;
    }
    let Some(local) = local else {
        return ScheduleAction::Wait;
    };
    match due_slot(times, now, local, last_started_at) {
        Some(_) => ScheduleAction::Run,
        None => ScheduleAction::Wait,
    }
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

pub(crate) fn unix_now() -> u64 {
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
pub(crate) fn run_command(
    home: &Path,
    trigger: NewsTrigger,
    model: Option<&str>,
    next_run_at: Option<u64>,
) -> String {
    let home = home.display().to_string();
    let mut command = format!(
        "python3 {} --home {} --trigger {} --deadline-min {RUN_BUDGET_MIN} --pinned",
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
    if let Some(next) = next_run_at {
        command.push_str(" --next-run ");
        command.push_str(&iso_utc(next));
    }
    command
}

/// The page viewer command (`news.open`): the latest page, or `edition`.
pub(crate) fn viewer_command(home: &Path, edition: Option<u32>) -> String {
    let viewer = home
        .join(crate::integration::news_assets::BIN_DIR)
        .join(VIEWER);
    let mut command = format!(
        "python3 {} {} --pinned",
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
    /// The scheduled times, minutes since local midnight, sorted.
    pub(crate) times: Vec<u16>,
    /// Quiet hours hold notifications only; the schedule ignores them.
    pub(crate) quiet: Option<QuietHours>,
    pub(crate) quiet_text: String,
    pub(crate) model: Option<String>,
    /// `<session data dir>/news`; `None` without a persisted session, and
    /// then `news.run` refuses.
    pub(crate) home: Option<PathBuf>,
    /// `news.json`; `None` keeps everything in memory (tests).
    pub(crate) store: Option<PathBuf>,
    /// When the last run started, any trigger: the schedule's memory (a
    /// slot at or before it is done). A failed scheduled start counts too,
    /// so the slot is not retried every tick.
    pub(crate) last_started_at: Option<u64>,
    pub(crate) tab_id: Option<String>,
    pub(crate) pane_id: Option<String>,
    pub(crate) run: Option<NewsRun>,
    pub(crate) consecutive_failures: u32,
    /// A viewer command waiting for the pane's shell prompt (`news.open`).
    pub(crate) pending_command: Option<PendingPaneCommand>,
    /// The notification ledger and queue (persisted).
    pub(crate) notify: store::NewsNotifyRecord,
    /// A delivery held back by the notification rate limit tries again then.
    pub(crate) notify_retry_at: Option<Instant>,
    /// The last edition the reader looked at (persisted; mirrored to the
    /// news home's read.json for the viewer).
    pub(crate) last_read_edition: Option<u32>,
    /// `new_stories` for news.get, cached per (latest edition, last read).
    pub(crate) new_count_cache: Option<((u32, u32), Option<u32>)>,
    /// Whether the News tab was the focused tab on the previous pass
    /// (not persisted: a restart sees the focus as new).
    pub(crate) was_focused: bool,
    /// Consecutive run-log polls that found the News pane back at a shell
    /// prompt while a run was in flight (the runner was interrupted).
    pub(crate) shell_polls: u8,
    /// News was just enabled (server start, a config reload turning it on):
    /// the next scheduler pass without a run in flight starts a first run
    /// when the desk has never run, then clears this either way.
    pub(crate) first_run_pending: bool,
    /// Tests: the local clock and day the policy goes by.
    #[cfg(test)]
    pub(crate) local_override: Option<(LocalClock, &'static str)>,
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
        state.first_run_pending = config.enabled;
        state.store = store;
        if let Some(store_path) = state.store.as_deref() {
            state.load_record(store::load(store_path), now);
            state.seed_schedule_memory(unix_now());
        }
        state
    }

    /// A record without `last_started_at` (written before the fixed-time
    /// schedule, or a desk that never ran): the schedule starts from the
    /// next slot rather than owing a slot it has no memory of.
    fn seed_schedule_memory(&mut self, now_unix: u64) {
        if self.last_started_at.is_none() {
            self.last_started_at = Some(now_unix);
            self.persist();
        }
    }

    /// State with nothing on disk but `home` (tests).
    pub(crate) fn in_memory(config: &NewsConfig, home: Option<PathBuf>) -> Self {
        let mut state = Self {
            enabled: false,
            times: Vec::new(),
            quiet: None,
            quiet_text: String::new(),
            model: None,
            home,
            store: None,
            last_started_at: None,
            tab_id: None,
            pane_id: None,
            run: None,
            consecutive_failures: 0,
            pending_command: None,
            notify: store::NewsNotifyRecord::default(),
            notify_retry_at: None,
            last_read_edition: None,
            new_count_cache: None,
            was_focused: false,
            shell_polls: 0,
            first_run_pending: false,
            #[cfg(test)]
            local_override: None,
            #[cfg(test)]
            assume_shell_ready: false,
            #[cfg(test)]
            assume_shell_busy: false,
        };
        state.apply_config(config);
        // Only a server start (`new`) or a later switch-on owes a first run.
        state.first_run_pending = false;
        state
    }

    pub(crate) fn apply_config(&mut self, config: &NewsConfig) {
        self.set_enabled(config.enabled);
        self.times = config.times();
        self.quiet = config.quiet_hours();
        self.quiet_text = config.quiet_hours.trim().to_string();
        self.model = config
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string);
    }

    /// Turn scheduling on or off; switching it on owes a first run to a
    /// desk that has never run (checked on the next scheduler pass).
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled {
            self.first_run_pending = enabled;
        }
        self.enabled = enabled;
    }

    /// Whether the desk has never run: no record in the run log and no
    /// published edition (what `news.status` and `news.history` read).
    /// Without a news home nothing can run, so it has not "never run".
    pub(crate) fn never_ran(&self) -> bool {
        self.home.as_deref().is_some_and(|home| {
            store::read_history(home, 1).is_empty() && store::read_editions(home).is_empty()
        })
    }

    fn load_record(&mut self, record: store::NewsRecord, now: Instant) {
        self.last_started_at = record.last_started_at;
        self.tab_id = record.tab_id;
        self.pane_id = record.pane_id;
        self.consecutive_failures = record.consecutive_failures;
        self.notify = record.notify;
        self.last_read_edition = record.last_read_edition;
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
            last_started_at: self.last_started_at,
            tab_id: self.tab_id.clone(),
            pane_id: self.pane_id.clone(),
            run: self.run.as_ref().map(NewsRun::persisted),
            consecutive_failures: self.consecutive_failures,
            notify: self.notify.clone(),
            last_read_edition: self.last_read_edition,
        }
    }

    /// The local clock and day the schedule and the notification policy go
    /// by (overridable in tests).
    pub(crate) fn local_now(&self) -> (Option<LocalClock>, String) {
        #[cfg(test)]
        if let Some((clock, day)) = self.local_override {
            return (Some(clock), day.to_string());
        }
        (LocalClock::now(), local_day_now())
    }

    /// When quiet hours end, when `local` is inside them (seconds since the
    /// epoch, from `now_unix`).
    fn quiet_end_unix(&self, now_unix: u64, local: Option<LocalClock>) -> Option<u64> {
        let (local, quiet) = (local?, self.quiet?);
        quiet.contains(local.minute_of_day).then(|| {
            let wait = u64::from(quiet.minutes_until_end(local.minute_of_day)) * 60;
            now_unix + wait.saturating_sub(u64::from(local.second))
        })
    }

    /// Queue a notification, replacing a queued one of the same kind.
    fn queue_notification(&mut self, pending: store::PendingNewsNotify) {
        let replaced = self.notify.pending.len();
        self.notify
            .pending
            .retain(|queued| queued.kind != pending.kind);
        tracing::info!(
            event = "news.notify",
            outcome = "queued",
            kind = %pending.kind,
            title = %pending.title,
            deliver_after = pending.deliver_after,
            replaced = replaced != self.notify.pending.len(),
            "news notification queued"
        );
        self.notify.pending.push(pending);
    }

    /// The first queued notification that may go out now.
    pub(crate) fn due_notification_index(&self, now_unix: u64) -> Option<usize> {
        self.notify
            .pending
            .iter()
            .position(|pending| pending.deliver_after <= now_unix)
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
            &self.times,
            self.run.is_some(),
            now,
            local,
            self.last_started_at,
        )
    }

    /// The next scheduled run as `news.status` and `news.get` report it:
    /// the slot owed now, else the upcoming one; `None` while scheduling is
    /// off, without a news home, without times or without a local clock.
    pub(crate) fn next_run_at(&self, now: u64, local: Option<LocalClock>) -> Option<u64> {
        if !self.enabled || self.home.is_none() {
            return None;
        }
        next_run_at(&self.times, now, local?, self.last_started_at)
    }

    /// The scheduled times as text, `HH:MM`.
    pub(crate) fn times_text(&self) -> Vec<String> {
        self.times.iter().copied().map(format_hhmm).collect()
    }

    /// The next instant the scheduler needs a tick: the run in flight (or
    /// the schedule), a pending viewer command, a notification held by the
    /// rate limit or waiting for quiet hours to end.
    pub(crate) fn next_deadline(
        &self,
        now: Instant,
        now_unix: u64,
        local: Option<LocalClock>,
    ) -> Option<Instant> {
        let mut deadlines = Vec::with_capacity(5);
        if self.first_run_pending && self.run.is_none() {
            deadlines.push(now);
        }
        if let Some(pending) = &self.pending_command {
            deadlines.push(pending.next_check);
        }
        if let Some(retry) = self.notify_retry_at.filter(|retry| *retry > now) {
            deadlines.push(retry);
        }
        if let Some(after) = self
            .notify
            .pending
            .iter()
            .map(|pending| pending.deliver_after)
            .filter(|after| *after > now_unix)
            .min()
        {
            deadlines.push(now + Duration::from_secs(after - now_unix));
        }
        if let Some(run) = &self.run {
            let watchdog = run.watch_from + WATCHDOG;
            deadlines.push(match run.phase {
                NewsPhase::Starting { next_check, .. } => next_check.min(watchdog),
                NewsPhase::Running { next_poll } => next_poll.min(watchdog),
                // Past the watchdog: the polls and the second Ctrl-C, never
                // the watchdog itself again.
                NewsPhase::Stopping {
                    next_poll,
                    second_interrupt,
                } => second_interrupt.map_or(next_poll, |second| second.min(next_poll)),
            });
        } else if let Some(next) = self.next_run_at(now_unix, local) {
            deadlines.push(now + Duration::from_secs(next.saturating_sub(now_unix)));
        }
        deadlines.into_iter().min()
    }

    /// The last finished run, from the run log.
    fn last_run(&self) -> Option<NewsRunRecord> {
        self.home
            .as_deref()
            .and_then(|home| store::read_history(home, 1).into_iter().next())
    }

    pub(crate) fn status(&self) -> NewsStatusInfo {
        let (local, _day) = self.local_now();
        NewsStatusInfo {
            enabled: self.enabled,
            times: self.times_text(),
            quiet_hours: self.quiet_text.clone(),
            model: self.model.clone(),
            home: self
                .home
                .as_ref()
                .map(|home| home.display().to_string())
                .unwrap_or_default(),
            next_run_at: self.next_run_at(unix_now(), local),
            tab_id: self.tab_id.clone(),
            pane_id: self.pane_id.clone(),
            run: self.run.as_ref().map(NewsRun::info),
            consecutive_failures: self.consecutive_failures,
            pending_notifications: self.notify.pending.len() as u32,
            recent: self
                .home
                .as_deref()
                .map(|home| store::read_history(home, store::MAX_HISTORY))
                .unwrap_or_default(),
        }
    }

    /// Account for a finished run: the failure counter, and the failure
    /// alert once the streak reaches [`FAILURE_ALERT_AFTER`] (queued for the
    /// end of quiet hours, `quiet_end`, when inside them).
    fn finish(
        &mut self,
        record: &NewsRunRecord,
        now_unix: u64,
        quiet_end: Option<u64>,
        user_cancel: bool,
    ) {
        self.run = None;
        if record.succeeded() {
            self.consecutive_failures = 0;
            self.notify.failure_alerted = false;
            return;
        }
        if user_cancel {
            // The reader stopped it (double Ctrl-C): not a failure of the
            // desk, the streak is neither reset nor extended.
            return;
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= FAILURE_ALERT_AFTER && !self.notify.failure_alerted {
            self.notify.failure_alerted = true;
            let error = record
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| record.outcome.clone());
            // The error text is the runner's (or the editor's): sanitized
            // like `notification.show` sanitizes its input.
            let body = super::api::sanitized_notification_text(
                &format!("{} in a row · {error}", self.consecutive_failures),
                NOTIFY_BODY_CHARS,
            );
            self.queue_notification(store::PendingNewsNotify {
                kind: PENDING_FAILURES.into(),
                title: "News runs failing".into(),
                body,
                high: false,
                deliver_after: quiet_end.unwrap_or(0),
                queued_at: now_unix,
            });
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
            let err = NewsError::Unavailable;
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

    pub(super) fn handle_news_set_quiet_hours(
        &mut self,
        id: String,
        params: crate::api::schema::NewsSetQuietHoursParams,
    ) -> String {
        match self.set_news_quiet_hours(&params.quiet_hours) {
            Ok(()) => encode_success(
                id,
                ResponseResult::NewsGet {
                    news: self.news_get_info(),
                },
            ),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    pub(super) fn handle_news_set_times(
        &mut self,
        id: String,
        params: NewsSetTimesParams,
    ) -> String {
        match self.set_news_times(&params.times) {
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
        let (local, _day) = self.news.local_now();
        NewsGetInfo {
            enabled: self.news.enabled,
            times: self.news.times_text(),
            quiet_hours: self.news.quiet_text.clone(),
            model: self.news.model.clone(),
            tab_id: pane
                .as_ref()
                .and_then(|pane| self.public_tab_id(pane.ws_idx, pane.tab_idx)),
            pane_id: pane
                .as_ref()
                .and_then(|pane| self.public_pane_id(pane.ws_idx, pane.pane_id)),
            next_run_at: self.news.next_run_at(unix_now(), local),
            run: self.news.run.as_ref().map(NewsRun::info),
            last_run: self.news.last_run().map(|record| record.last_run()),
            unread,
            consecutive_failures: self.news.consecutive_failures,
            pending_notifications: self.news.notify.pending.len() as u32,
            last_read_edition: self.news.last_read_edition,
            new_stories: self.news_new_story_count(),
        }
    }

    /// The News tab as a notification target: its space, tab and pane ids.
    pub(crate) fn news_notification_target(&self) -> Option<(String, String, String)> {
        let pane = self.existing_news_pane()?;
        Some((
            self.public_workspace_id(pane.ws_idx),
            self.public_tab_id(pane.ws_idx, pane.tab_idx)?,
            self.public_pane_id(pane.ws_idx, pane.pane_id)?,
        ))
    }

    /// `news.set_enabled`: write `news.enabled` to the config file and reload
    /// it, so the file stays the truth and the running server follows.
    pub(crate) fn set_news_enabled(&mut self, enabled: bool) -> Result<(), NewsError> {
        crate::config::write_edit(crate::config::ConfigEdit::NewsEnabled(enabled))
            .map_err(NewsError::ConfigWrite)?;
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
            self.news.set_enabled(enabled);
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

    /// `news.set_times`: validate the times, write their canonical form
    /// (sorted, `HH:MM`, no duplicates) to `news.times` in the config file
    /// and reload it. The schedule follows at once: `next_run_at` is derived
    /// from the list and the last run's start, never stored.
    pub(crate) fn set_news_times(&mut self, times: &[String]) -> Result<(), NewsError> {
        let minutes =
            normalize_times(times.iter().map(String::as_str)).map_err(NewsError::InvalidTime)?;
        let texts: Vec<String> = minutes.iter().copied().map(format_hhmm).collect();
        crate::config::write_edit(crate::config::ConfigEdit::NewsTimes(&texts))
            .map_err(NewsError::ConfigWrite)?;
        let report = self.reload_config();
        if self.news.times != minutes {
            tracing::warn!(
                event = "news.set_times",
                outcome = "applied_directly",
                status = ?report.status,
                "config reload did not apply news.times; applying it in memory"
            );
            self.news.times = minutes;
        }
        tracing::info!(
            event = "news.set_times",
            outcome = "ok",
            times = %texts.join(" "),
            "news schedule changed"
        );
        Ok(())
    }

    /// `news.set_quiet_hours`: validate the window, write its canonical form
    /// (`HH:MM-HH:MM`, or empty for none) to `news.quiet_hours` in the config
    /// file and reload it — the settings section's quiet-hours row goes
    /// through here, so a remote server's own config changes.
    pub(crate) fn set_news_quiet_hours(&mut self, text: &str) -> Result<(), NewsError> {
        let parsed =
            crate::config::parse_quiet_hours(text).map_err(NewsError::InvalidQuietHours)?;
        let canonical = parsed
            .map(|q| format!("{}-{}", format_hhmm(q.start), format_hhmm(q.end)))
            .unwrap_or_default();
        crate::config::write_edit(crate::config::ConfigEdit::NewsQuietHours(&canonical))
            .map_err(NewsError::ConfigWrite)?;
        let report = self.reload_config();
        if self.news.quiet != parsed {
            tracing::warn!(
                event = "news.set_quiet_hours",
                outcome = "applied_directly",
                status = ?report.status,
                "config reload did not apply news.quiet_hours; applying it in memory"
            );
            self.news.quiet = parsed;
            self.news.quiet_text = canonical.clone();
        }
        tracing::info!(
            event = "news.set_quiet_hours",
            outcome = "ok",
            quiet_hours = %canonical,
            "news quiet hours changed"
        );
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
    ) -> Result<(), NewsError> {
        if edition.is_some() && self.news.run.is_some() {
            return Err(NewsError::InFlight);
        }
        let home = self.news.home.clone().ok_or(NewsError::Unavailable)?;
        if let Some(edition) = edition {
            if !store::read_editions(&home)
                .iter()
                .any(|entry| entry.edition == edition)
            {
                return Err(NewsError::NoEdition(edition));
            }
        }
        let (pane, created) = self.ensure_news_tab(&home).map_err(NewsError::Failed)?;
        self.state.switch_workspace_tab(pane.ws_idx, pane.tab_idx);
        self.schedule_session_save();
        let show_viewer = edition.is_some()
            || (created && self.news.run.is_none() && store::page_path(&home).is_file());
        if show_viewer {
            std::fs::create_dir_all(&home)
                .and_then(|()| crate::integration::news_assets::install(&home))
                .map_err(|err| {
                    NewsError::Failed(format!(
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
        let ready = self.news_pane_at_shell(&pane);
        if ready {
            self.news.pending_command = None;
            let mut command = String::from(KILL_LINE);
            command.push_str(&pending.command);
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

    /// "Since you read": while the News tab is the focused tab of the focused
    /// space and its pane shows the viewer (not a shell prompt), the edition
    /// the viewer reports on screen (`viewer-state.json`) becomes the last
    /// read edition — never moving backwards — persisted in news.json and
    /// mirrored to `read.json` for the viewer's baseline. Returns whether it
    /// moved.
    fn note_news_read_when_focused(&mut self) -> bool {
        let Some(pane) = self.focused_news_pane() else {
            return false;
        };
        let Some(home) = self.news.home.clone() else {
            return false;
        };
        if self.news_pane_at_shell(&pane) {
            return false;
        }
        let Some(showing) = store::viewer_showing(&home) else {
            return false;
        };
        if self
            .news
            .last_read_edition
            .is_some_and(|read| read >= showing)
        {
            return false;
        }
        self.news.last_read_edition = Some(showing);
        self.news.new_count_cache = None;
        if let Err(err) = store::write_read_record(&home, showing) {
            tracing::warn!(event = "news.read", outcome = "mirror_failed", err = %err, "could not write read.json");
        }
        tracing::info!(event = "news.read", edition = showing, "News edition read");
        self.news.persist();
        true
    }

    /// `new_stories` for news.get: stories in the latest edition that were
    /// not in the last one read; cached per pair.
    fn news_new_story_count(&self) -> Option<u32> {
        let home = self.news.home.as_deref()?;
        let read = self.news.last_read_edition?;
        let latest = store::read_editions(home).last()?.edition;
        if let Some((key, count)) = self.news.new_count_cache {
            if key == (latest, read) {
                return count;
            }
        }
        store::new_story_count(home, latest, read)
    }

    /// The scheduler's contribution to the headless loop deadline.
    pub(crate) fn next_news_deadline(&self, now: Instant) -> Option<Instant> {
        let (local, _day) = self.news.local_now();
        self.news.next_deadline(now, unix_now(), local)
    }

    /// One scheduler pass: clear the unread mark of a focused News tab,
    /// drive the run in flight (launch, poll, watchdog) or the pending
    /// viewer command, or start a first run (news just enabled on a desk
    /// that never ran) or a due scheduled run. Returns whether shared state
    /// changed.
    pub(crate) fn handle_news_tasks(&mut self, now: Instant) -> bool {
        let unread_cleared = self.clear_news_unread_when_focused();
        let read = self.note_news_read_when_focused();
        let restored = self.show_page_when_news_focused(now);
        let changed = unread_cleared || read || restored;
        if self.news.run.is_some() {
            self.news.pending_command = None;
            return self.drive_news_run(now) || changed;
        }
        self.drive_news_pending_command(now);
        if std::mem::take(&mut self.news.first_run_pending)
            && self.news.enabled
            && self.news.never_ran()
        {
            tracing::info!(
                event = "news.schedule",
                outcome = "first_run",
                "news enabled on a desk that never ran; starting the first run"
            );
            return self.start_scheduled_news_run(now, "first") || changed;
        }
        let (local, _day) = self.news.local_now();
        match self.news.schedule_action(unix_now(), local) {
            ScheduleAction::Wait => changed,
            ScheduleAction::Run => self.start_scheduled_news_run(now, "slot") || changed,
        }
    }

    /// Start a run the server decided on (`reason`: the first run or a due
    /// slot). A start that fails stands for the attempt: the next listed
    /// time tries again, not every tick.
    fn start_scheduled_news_run(&mut self, now: Instant, reason: &'static str) -> bool {
        match self.start_news_run(NewsTrigger::Scheduled, now) {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(
                    event = "news.schedule",
                    outcome = "start_failed",
                    reason,
                    code = err.code(),
                    err = %err.message(),
                    "scheduled news run did not start; the slot counts as attempted"
                );
                self.news.last_started_at = Some(unix_now());
                self.news.persist();
                false
            }
        }
    }

    /// Start a run: install the runner, ensure the News tab, type the
    /// command (or `q` first when the pane is busy), record the run.
    pub(crate) fn start_news_run(
        &mut self,
        trigger: NewsTrigger,
        now: Instant,
    ) -> Result<NewsRunInfo, NewsError> {
        if self.news.run.is_some() {
            return Err(NewsError::InFlight);
        }
        let home = self.news.home.clone().ok_or(NewsError::Unavailable)?;
        std::fs::create_dir_all(&home)
            .and_then(|()| crate::integration::news_assets::install(&home))
            .map_err(|err| {
                NewsError::Failed(format!(
                    "failed to install the news runner under {}: {err}",
                    home.display()
                ))
            })?;
        let (pane, _) = self.ensure_news_tab(&home).map_err(NewsError::Failed)?;
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
        // Any run stands for the slots reached so far today; the listed
        // times themselves never move.
        self.news.last_started_at = Some(started_at);
        tracing::info!(
            event = "news.run",
            outcome = "started",
            trigger = trigger.name(),
            tab_id = self.news.tab_id.as_deref().unwrap_or(""),
            "news run started"
        );
        self.launch_news_run(&pane, now);
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

    /// Whether the News pane's foreground process is its shell (a prompt).
    fn news_pane_at_shell(&self, pane: &NewsPane) -> bool {
        #[allow(unused_mut)] // the test override below assigns it
        let mut ready = self
            .lookup_runtime_sender(pane.ws_idx, pane.pane_id)
            .is_some_and(|runtime| super::agents::available_shell_name(runtime).is_some());
        #[cfg(test)]
        {
            ready = (ready || self.news.assume_shell_ready) && !self.news.assume_shell_busy;
        }
        ready
    }

    /// The News pane, when its tab is the focused tab of the focused space.
    fn focused_news_pane(&self) -> Option<NewsPane> {
        let pane = self.existing_news_pane()?;
        let focused = self.state.active == Some(pane.ws_idx)
            && self
                .state
                .workspaces
                .get(pane.ws_idx)
                .is_some_and(|ws| ws.active_tab == pane.tab_idx);
        focused.then_some(pane)
    }

    /// When the News tab gains focus and its pane sits at a bare shell (a
    /// server restart restores panes as fresh shells; a viewer that was
    /// quit leaves one), show the latest page again. Nothing while a run is
    /// in flight or a command is already pending.
    fn show_page_when_news_focused(&mut self, now: Instant) -> bool {
        let focused = self.focused_news_pane();
        let gained = focused.is_some() && !self.news.was_focused;
        self.news.was_focused = focused.is_some();
        let (Some(pane), true) = (focused, gained) else {
            return false;
        };
        if self.news.run.is_some() || self.news.pending_command.is_some() {
            return false;
        }
        let Some(home) = self.news.home.clone() else {
            return false;
        };
        if !store::page_path(&home).is_file() || !self.news_pane_at_shell(&pane) {
            return false;
        }
        if let Err(err) = crate::integration::news_assets::install(&home) {
            tracing::warn!(
                event = "news.open",
                outcome = "install_failed",
                err = %err,
                "could not install the news viewer"
            );
            return false;
        }
        tracing::info!(
            event = "news.open",
            outcome = "restored",
            "News tab focused at a bare shell; showing the page"
        );
        self.news.pending_command = Some(PendingPaneCommand {
            command: viewer_command(&home, None),
            next_check: now,
            give_up_at: now + START_TIMEOUT,
            quit_sent: true,
        });
        self.drive_news_pending_command(now)
    }

    fn news_pane_bytes(&self, pane: &NewsPane, bytes: Bytes) -> Result<(), String> {
        let runtime = self
            .lookup_runtime_sender(pane.ws_idx, pane.pane_id)
            .ok_or_else(|| "the News pane has no shell".to_string())?;
        runtime.try_send_bytes(bytes).map_err(|err| err.to_string())
    }

    /// Type the command when the pane is at a shell prompt; otherwise send
    /// the viewer's quit sequence (on every attempt: a split or lost one
    /// must not cost the start) and keep the run in `Starting`.
    fn launch_news_run(&mut self, pane: &NewsPane, now: Instant) {
        let ready = self.news_pane_at_shell(pane);
        let (trigger, give_up_at) = match self.news.run.as_ref() {
            Some(run) => match run.phase {
                NewsPhase::Starting { give_up_at, .. } => (run.trigger, give_up_at),
                NewsPhase::Running { .. } | NewsPhase::Stopping { .. } => return,
            },
            None => return,
        };
        if ready {
            self.clear_stale_news_identity(pane);
            let home = self.news.home.clone().unwrap_or_default();
            let (local, _day) = self.news.local_now();
            let next = self.news.next_run_at(unix_now(), local);
            let mut command = String::from(KILL_LINE);
            command.push_str(&run_command(
                &home,
                trigger,
                self.news.model.as_deref(),
                next,
            ));
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
        if let Err(err) = self.news_pane_bytes(pane, Bytes::from_static(VIEWER_QUIT)) {
            self.fail_news_run(format!("could not reach the News pane: {err}"));
            return;
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
    /// running, time out past the watchdog; a stopping run keeps polling
    /// for the runner's record or the pane's prompt (and sends the
    /// fallback's second Ctrl-C).
    fn drive_news_run(&mut self, now: Instant) -> bool {
        let Some(run) = self.news.run.clone() else {
            return false;
        };
        let pane = self.existing_news_pane();
        match run.phase {
            NewsPhase::Starting { next_check, .. } => {
                if now >= run.watch_from + WATCHDOG {
                    self.time_out_news_run(now);
                    return true;
                }
                if now < next_check {
                    return false;
                }
                match pane {
                    Some(pane) => self.launch_news_run(&pane, now),
                    None => self.fail_news_run("the News tab is gone".into()),
                }
                self.news.persist();
                true
            }
            NewsPhase::Running { next_poll } => {
                if now >= run.watch_from + WATCHDOG {
                    self.time_out_news_run(now);
                    return true;
                }
                if now < next_poll {
                    return false;
                }
                if self.poll_news_run(&run, pane.as_ref()) {
                    return true;
                }
                if let Some(run) = self.news.run.as_mut() {
                    run.phase = NewsPhase::Running {
                        next_poll: now + POLL_INTERVAL,
                    };
                }
                false
            }
            NewsPhase::Stopping {
                next_poll,
                second_interrupt,
            } => {
                let mut changed = false;
                let mut second_interrupt = second_interrupt;
                if second_interrupt.is_some_and(|due| now >= due) {
                    second_interrupt = None;
                    changed = true;
                    if let Some(pane) = pane.as_ref() {
                        if let Err(err) = self.news_pane_bytes(pane, Bytes::from_static(INTERRUPT))
                        {
                            tracing::warn!(
                                event = "news.run",
                                outcome = "interrupt_failed",
                                err = %err,
                                "could not send the second Ctrl-C to the news run"
                            );
                        }
                    }
                }
                let mut next_poll = next_poll;
                if now >= next_poll {
                    if self.poll_news_run(&run, pane.as_ref()) {
                        return true;
                    }
                    next_poll = now + POLL_INTERVAL;
                }
                if let Some(run) = self.news.run.as_mut() {
                    run.phase = NewsPhase::Stopping {
                        next_poll,
                        second_interrupt,
                    };
                }
                changed
            }
        }
    }

    /// One poll of the run log for the run in flight. Its record completes
    /// it. The runner writes its record before it exits, so a pane back at
    /// its shell prompt with no record means the runner is gone: after
    /// [`INTERRUPTED_AFTER_POLLS`] such polls in a row the run ends as
    /// `interrupted`, or as `timeout` when the watchdog had stopped it. A
    /// missing pane counts as at the shell (nothing runs there). Returns
    /// whether the run ended.
    fn poll_news_run(&mut self, run: &NewsRun, pane: Option<&NewsPane>) -> bool {
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
            self.news.shell_polls = 0;
            self.complete_news_run(record);
            return true;
        }
        let at_shell = pane.is_none_or(|pane| self.news_pane_at_shell(pane));
        self.news.shell_polls = if at_shell {
            self.news.shell_polls.saturating_add(1)
        } else {
            0
        };
        if self.news.shell_polls >= INTERRUPTED_AFTER_POLLS {
            self.news.shell_polls = 0;
            if matches!(run.phase, NewsPhase::Stopping { .. }) {
                self.finish_news_run_timeout();
            } else {
                self.interrupt_news_run();
            }
            return true;
        }
        false
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

    /// The runner stopped without writing its record (Ctrl-C, a crash): the
    /// agent released, an `interrupted` record, and the page shown again.
    fn interrupt_news_run(&mut self) {
        let Some(run) = self.news.run.clone() else {
            return;
        };
        tracing::warn!(
            event = "news.run",
            outcome = "interrupted",
            trigger = run.trigger.name(),
            "news runner exited without a result"
        );
        if let Some(pane) = self.existing_news_pane() {
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
            "interrupted",
            "the runner exited without a result".into(),
        );
        self.complete_news_run(record);
        // Bring the page back: the pane is at a bare shell now.
        self.news.was_focused = false;
    }

    /// The watchdog: the runner is told to stop and the run goes
    /// `Stopping`. SIGTERM to the News pane's foreground process group (the
    /// runner's handler takes its interrupt path: the editor's group is
    /// killed, `interrupted` is recorded, the page comes back); when no
    /// process can be found, Ctrl-C now and once more after
    /// [`INTERRUPT_GAP`] on a later pass (the pinned runner wants two). The
    /// run then ends with the runner's record, or with a `timeout` record
    /// once the pane is back at its prompt ([`Self::poll_news_run`]).
    fn time_out_news_run(&mut self, now: Instant) {
        let Some(run) = self.news.run.clone() else {
            return;
        };
        let pane = self.existing_news_pane();
        let pgid = pane
            .as_ref()
            .and_then(|pane| self.news_pane_foreground_pgid(pane));
        let mut second_interrupt = None;
        let method = match (pgid, pane.as_ref()) {
            (Some(pgid), _) => {
                crate::platform::signal_process_group(pgid, crate::platform::Signal::Terminate);
                "sigterm"
            }
            (None, Some(pane)) => match self.news_pane_bytes(pane, Bytes::from_static(INTERRUPT)) {
                Ok(()) => {
                    second_interrupt = Some(now + INTERRUPT_GAP);
                    "ctrl_c"
                }
                Err(err) => {
                    tracing::warn!(
                        event = "news.run",
                        outcome = "interrupt_failed",
                        err = %err,
                        "could not interrupt the news run"
                    );
                    "none"
                }
            },
            (None, None) => "none",
        };
        tracing::warn!(
            event = "news.run",
            outcome = "watchdog",
            trigger = run.trigger.name(),
            method,
            pgid = pgid.unwrap_or(0),
            "news run exceeded the watchdog; stopping it"
        );
        if let Some(run) = self.news.run.as_mut() {
            run.phase = NewsPhase::Stopping {
                next_poll: now + POLL_INTERVAL,
                second_interrupt,
            };
        }
        self.news.shell_polls = 0;
        self.news.persist();
    }

    /// The News pane's foreground process group while something other than
    /// its shell runs there (the runner's job); `None` at a prompt, so the
    /// shell itself is never signalled.
    fn news_pane_foreground_pgid(&self, pane: &NewsPane) -> Option<u32> {
        if self.news_pane_at_shell(pane) {
            return None;
        }
        let runtime = self.lookup_runtime_sender(pane.ws_idx, pane.pane_id)?;
        crate::detect::foreground_process_group_id(runtime.child_pid()?)
    }

    /// A stopped run whose runner never wrote its record: the agent
    /// released, a `timeout` record, and the page shown again.
    fn finish_news_run_timeout(&mut self) {
        let Some(run) = self.news.run.clone() else {
            return;
        };
        tracing::warn!(
            event = "news.run",
            outcome = "timeout",
            trigger = run.trigger.name(),
            "news runner stopped without a result"
        );
        if let Some(pane) = self.existing_news_pane() {
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
        self.news.was_focused = false;
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

    /// Account for the finished run, mark the tab when the page changed,
    /// and queue the editor's notification (its text sanitized like
    /// `notification.show` input; the daily cap is applied at delivery).
    fn complete_news_run(&mut self, record: NewsRunRecord) {
        tracing::info!(
            event = "news.run",
            outcome = %record.outcome,
            edition = record.edition.unwrap_or(0),
            cost_usd = record.cost_usd,
            turns = record.turns,
            changed = record.changed,
            notify = record.notify.is_some(),
            "news run finished"
        );
        let now_unix = unix_now();
        let (local, _day) = self.news.local_now();
        let quiet_end = self.news.quiet_end_unix(now_unix, local);
        // The runner's own `interrupted` outside the watchdog's Stopping phase
        // is the reader's double Ctrl-C; herdr's records and a watchdog stop
        // keep counting.
        let user_cancel = record.outcome == "interrupted"
            && record.errors.iter().any(|e| e == "interrupted by the user")
            && !matches!(
                self.news.run.as_ref().map(|run| &run.phase),
                Some(NewsPhase::Stopping { .. })
            );
        self.news.finish(&record, now_unix, quiet_end, user_cancel);
        self.news.new_count_cache = None;
        if record.outcome == "ok" && record.changed {
            self.mark_news_tab_important();
            if let Some(notify) = record.notify.as_ref() {
                let title = super::api::sanitized_notification_text(
                    &notify.title,
                    NOTIFY_TITLE_CHARS - NEWS_TITLE_PREFIX.len(),
                );
                // "N new" against the last edition read, ahead of the editor's body.
                let new_count = record
                    .edition
                    .zip(self.news.last_read_edition)
                    .zip(self.news.home.as_deref())
                    .and_then(|((edition, read), home)| store::new_story_count(home, edition, read))
                    .filter(|n| *n > 0);
                let body = match (new_count, notify.body.as_deref()) {
                    (Some(n), Some(body)) => Some(format!("{n} new · {body}")),
                    (Some(n), None) => Some(format!("{n} new")),
                    (None, body) => body.map(str::to_string),
                };
                match title {
                    Some(title) => self.news.queue_notification(store::PendingNewsNotify {
                        kind: PENDING_RUN.into(),
                        title: format!("{NEWS_TITLE_PREFIX}{title}"),
                        body: body.as_deref().and_then(|body| {
                            super::api::sanitized_notification_text(body, NOTIFY_BODY_CHARS)
                        }),
                        high: notify.urgency == "high",
                        deliver_after: quiet_end.unwrap_or(0),
                        queued_at: now_unix,
                    }),
                    None => tracing::info!(
                        event = "news.notify",
                        outcome = "dropped",
                        reason = "empty title",
                        "news notification dropped: nothing left of the title once sanitized"
                    ),
                }
            }
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

    fn clock(hour: u16, minute: u16, second: u8) -> Option<LocalClock> {
        Some(LocalClock {
            minute_of_day: hour * 60 + minute,
            second,
        })
    }

    const NOW: u64 = 1_800_000_000;
    /// The default times, minutes since local midnight.
    const TIMES: &[u16] = &[8 * 60, 13 * 60, 19 * 60];
    /// Local midnight when the fixed clock reads noon at `NOW`.
    const MIDNIGHT: u64 = NOW - 12 * 3600;

    /// `NOW` shifted to the fixed clock's `hour`, on the same local day.
    fn at(hour: u64, minute: u64) -> u64 {
        MIDNIGHT + hour * 3600 + minute * 60
    }

    #[test]
    fn the_next_slot_is_later_today_else_the_first_tomorrow() {
        let noon = clock(12, 0, 0).unwrap();
        assert_eq!(upcoming_slot(TIMES, NOW, noon), Some(at(13, 0)));
        assert_eq!(
            next_run_at(TIMES, NOW, noon, Some(at(8, 1))),
            Some(at(13, 0)),
            "the 08:00 slot ran; 13:00 is next"
        );
        let evening = clock(20, 0, 0).unwrap();
        assert_eq!(
            upcoming_slot(TIMES, at(20, 0), evening),
            Some(at(8, 0) + 86_400),
            "past the last time: the first one tomorrow"
        );
        assert_eq!(
            next_run_at(TIMES, at(20, 0), evening, Some(at(19, 0))),
            Some(at(8, 0) + 86_400)
        );
        let one_pm = clock(13, 0, 0).unwrap();
        assert_eq!(
            next_run_at(TIMES, at(13, 0), one_pm, Some(at(12, 59))),
            Some(at(13, 0)),
            "at the slot itself it is due"
        );
        assert_eq!(upcoming_slot(&[], NOW, noon), None);
        assert_eq!(
            next_run_at(&[], NOW, noon, None),
            None,
            "an empty list waits"
        );
        assert_eq!(
            next_run_at(
                &[7 * 60 + 30],
                at(23, 59) + 59,
                clock(23, 59, 59).unwrap(),
                Some(at(7, 30))
            ),
            Some(at(7, 30) + 86_400)
        );
    }

    #[test]
    fn a_slot_reached_today_runs_once_unless_a_run_started_since_it() {
        let two_pm = clock(14, 0, 0);
        assert_eq!(
            schedule_action(true, TIMES, false, at(14, 0), two_pm, Some(at(8, 1))),
            ScheduleAction::Run,
            "13:00 was missed (the server was off) and nothing ran since"
        );
        assert_eq!(
            due_slot(TIMES, at(14, 0), two_pm.unwrap(), Some(at(8, 1))),
            Some(at(13, 0))
        );
        assert_eq!(
            schedule_action(true, TIMES, false, at(14, 0), two_pm, Some(at(13, 0) + 5)),
            ScheduleAction::Wait,
            "a run started since the slot: nothing owed"
        );
        assert_eq!(
            schedule_action(true, TIMES, false, at(14, 0), two_pm, Some(at(12, 30))),
            ScheduleAction::Run,
            "a manual run before the slot does not stand for it"
        );
        assert_eq!(
            due_slot(TIMES, at(14, 0), two_pm.unwrap(), Some(at(8, 0) - 86_400)),
            Some(at(13, 0)),
            "08:00 and 13:00 both missed: one run, for the latest"
        );
        assert_eq!(
            due_slot(TIMES, at(14, 0), two_pm.unwrap(), None),
            Some(at(13, 0)),
            "no memory at all: today's latest slot"
        );
        let seven_am = clock(7, 0, 0);
        assert_eq!(
            schedule_action(
                true,
                TIMES,
                false,
                at(7, 0),
                seven_am,
                Some(at(8, 0) - 86_400)
            ),
            ScheduleAction::Wait,
            "yesterday's 19:00 was missed but is not today's"
        );
        assert_eq!(
            next_run_at(TIMES, at(7, 0), seven_am.unwrap(), None),
            Some(at(8, 0))
        );
        assert_eq!(
            schedule_action(
                true,
                TIMES,
                false,
                at(13, 0),
                clock(13, 0, 0),
                Some(at(12, 59))
            ),
            ScheduleAction::Run,
            "the slot itself"
        );
        assert_eq!(
            schedule_action(
                true,
                TIMES,
                false,
                at(12, 59),
                clock(12, 59, 59),
                Some(at(8, 0))
            ),
            ScheduleAction::Wait,
            "a second before"
        );
    }

    #[test]
    fn schedule_is_inert_while_disabled_in_flight_empty_or_without_a_clock() {
        let two_pm = clock(14, 0, 0);
        assert_eq!(
            schedule_action(false, TIMES, false, at(14, 0), two_pm, None),
            ScheduleAction::Wait,
            "disabled waits"
        );
        assert_eq!(
            schedule_action(true, TIMES, true, at(14, 0), two_pm, None),
            ScheduleAction::Wait,
            "in flight waits"
        );
        assert_eq!(
            schedule_action(true, &[], false, at(14, 0), two_pm, None),
            ScheduleAction::Wait,
            "an empty list waits"
        );
        assert_eq!(
            schedule_action(true, TIMES, false, at(14, 0), None, None),
            ScheduleAction::Wait,
            "no local clock: the slots cannot be placed"
        );
    }

    #[test]
    fn a_record_without_schedule_memory_starts_from_the_next_slot() {
        let dir = temp_home("seed");
        let store_path = dir.join(store::FILE_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &store_path,
            "{\"version\":1,\"next_run_at\":1790692288,\"consecutive_failures\":0}",
        )
        .unwrap();
        let mut state = NewsState::in_memory(&NewsConfig::default(), Some(dir.clone()));
        state.store = Some(store_path.clone());
        state.load_record(store::load(&store_path), Instant::now());
        assert_eq!(state.last_started_at, None, "the old file has no memory");
        state.seed_schedule_memory(at(14, 0));
        assert_eq!(state.last_started_at, Some(at(14, 0)));
        assert_eq!(
            store::load(&store_path).last_started_at,
            Some(at(14, 0)),
            "the seed is written, so a second boot keeps it"
        );
        assert_eq!(
            state.schedule_action(at(14, 0), clock(14, 0, 0)),
            ScheduleAction::Wait,
            "scheduling is off in the default config"
        );
        state.enabled = true;
        assert_eq!(
            state.schedule_action(at(14, 0), clock(14, 0, 0)),
            ScheduleAction::Wait,
            "13:00 is not owed: the desk has no memory of it"
        );
        assert_eq!(
            state.next_run_at(at(14, 0), clock(14, 0, 0)),
            Some(at(19, 0))
        );
        let mut kept = NewsState::in_memory(&NewsConfig::default(), Some(dir.clone()));
        kept.store = Some(store_path.clone());
        kept.load_record(
            store::NewsRecord {
                last_started_at: Some(at(8, 1)),
                ..store::NewsRecord::default()
            },
            Instant::now(),
        );
        kept.seed_schedule_memory(at(14, 0));
        assert_eq!(
            kept.last_started_at,
            Some(at(8, 1)),
            "a record with memory keeps it"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
            run_command(home, NewsTrigger::Manual, None, None),
            "python3 '/tmp/it'\\''s news/bin/news_run.py' --home '/tmp/it'\\''s news' --trigger manual --deadline-min 60 --pinned"
        );
        assert_eq!(
            run_command(Path::new("/n"), NewsTrigger::Scheduled, Some(" opus "), Some(1_790_000_000)),
            "python3 '/n/bin/news_run.py' --home '/n' --trigger scheduled --deadline-min 60 --pinned --model 'opus' --next-run 2026-09-21T14:13:20+00:00"
        );
        assert_eq!(
            run_command(Path::new("/n"), NewsTrigger::Scheduled, Some("  "), None),
            "python3 '/n/bin/news_run.py' --home '/n' --trigger scheduled --deadline-min 60 --pinned"
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
            Err(NewsError::InFlight)
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

    /// A manual run leaves the listed times alone (the next run stays the
    /// next listed time); a scheduled run stands for its slot, so the run
    /// after it is the following listed time.
    #[tokio::test]
    async fn a_manual_run_does_not_move_the_schedule_but_a_scheduled_run_takes_its_slot() {
        let home = temp_home("manual-run");
        let mut app = news_app(Some(home.clone()), true);
        app.news.assume_shell_ready = true;
        let noon = clock(12, 0, 0);
        app.news.local_override = Some((noon.unwrap(), "2026-09-29"));
        app.news.last_started_at = Some(unix_now() - 60);
        let before = app.news.next_run_at(unix_now(), noon).expect("13:00");
        assert!(before.abs_diff(unix_now() + 3600) <= 2, "{before}");
        assert_eq!(
            app.news.schedule_action(unix_now(), noon),
            ScheduleAction::Wait,
            "nothing is owed at noon"
        );
        app.start_news_run(NewsTrigger::Manual, Instant::now())
            .expect("manual run");
        let started = app.news.last_started_at.expect("remembered");
        assert!(started.abs_diff(unix_now()) <= 2);
        let after = app.news.next_run_at(unix_now(), noon).expect("still 13:00");
        assert!(
            after.abs_diff(before) <= 2,
            "the next run is still the 13:00 slot: {before} -> {after}"
        );

        // The manual run is over; at 14:00 the 13:00 slot has passed since
        // the last start, so the scheduler runs it and 19:00 follows.
        app.news.run = None;
        let two_pm = clock(14, 0, 0);
        app.news.local_override = Some((two_pm.unwrap(), "2026-09-29"));
        // Two hours ago on the 14:00 clock is 12:00, before the slot.
        app.news.last_started_at = Some(unix_now() - 2 * 3600);
        assert_eq!(
            app.news.schedule_action(unix_now(), two_pm),
            ScheduleAction::Run
        );
        assert!(app.handle_news_tasks(Instant::now()));
        let run = app.news.run.as_ref().expect("a scheduled run started");
        assert_eq!(run.trigger, NewsTrigger::Scheduled);
        let next = app.news.next_run_at(unix_now(), two_pm).expect("19:00");
        assert!(
            next.abs_diff(unix_now() + 5 * 3600) <= 2,
            "the following slot: {next}"
        );
        assert_eq!(
            app.news.schedule_action(unix_now(), two_pm),
            ScheduleAction::Wait,
            "in flight"
        );
        app.news.run = None;
        assert_eq!(
            app.news.schedule_action(unix_now(), two_pm),
            ScheduleAction::Wait,
            "the slot is done; nothing runs again before 19:00"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A scheduled start that fails (here: the runner cannot be installed
    /// because the home sits under a regular file) stands for its slot, so
    /// the scheduler does not retry every tick; the next listed time tries
    /// again.
    #[test]
    fn a_failed_scheduled_start_counts_as_the_slots_attempt() {
        let dir = temp_home("start-fails");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let mut app = news_app(Some(blocker.join("news")), true);
        let two_pm = clock(14, 0, 0);
        app.news.local_override = Some((two_pm.unwrap(), "2026-09-29"));
        app.news.last_started_at = Some(unix_now() - 2 * 3600);
        assert_eq!(
            app.news.schedule_action(unix_now(), two_pm),
            ScheduleAction::Run,
            "13:00 is owed at 14:00"
        );
        assert!(!app.handle_news_tasks(Instant::now()), "nothing started");
        assert!(app.news.run.is_none());
        let attempted = app.news.last_started_at.expect("the attempt is remembered");
        assert!(attempted.abs_diff(unix_now()) <= 2, "{attempted}");
        assert_eq!(
            app.news.schedule_action(unix_now(), two_pm),
            ScheduleAction::Wait,
            "not retried on the next tick"
        );
        let next = app.news.next_run_at(unix_now(), two_pm).expect("19:00");
        assert!(next.abs_diff(unix_now() + 5 * 3600) <= 2, "{next}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An app whose schedule owes nothing (noon, a start a minute ago), so
    /// only the first-run rule can start a run.
    fn first_run_app(home: Option<PathBuf>) -> App {
        let mut app = news_app(home, false);
        app.news.assume_shell_ready = true;
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        app.news.last_started_at = Some(unix_now() - 60);
        app
    }

    fn enabled_config() -> NewsConfig {
        NewsConfig {
            enabled: true,
            ..NewsConfig::default()
        }
    }

    /// Server start with news enabled owes a first run; `in_memory` (tests)
    /// and a disabled start do not.
    #[test]
    fn a_server_start_with_news_enabled_owes_a_first_run() {
        let now = Instant::now();
        assert!(NewsState::new(&enabled_config(), false, now).first_run_pending);
        assert!(!NewsState::new(&NewsConfig::default(), false, now).first_run_pending);
        assert!(!NewsState::in_memory(&enabled_config(), None).first_run_pending);
    }

    /// Enabling news on a desk that never ran starts a run on the next
    /// pass, before any listed time, and only once.
    #[tokio::test]
    async fn enabling_news_on_a_desk_that_never_ran_starts_a_first_run_once() {
        let home = temp_home("first-run");
        let mut app = first_run_app(Some(home.clone()));
        let (local, _) = app.news.local_now();
        assert_eq!(
            app.news.schedule_action(unix_now(), local),
            ScheduleAction::Wait,
            "no slot is owed at noon"
        );
        app.news.apply_config(&enabled_config());
        assert!(app.news.first_run_pending);
        let now = Instant::now();
        assert_eq!(
            app.next_news_deadline(now),
            Some(now),
            "the loop wakes at once"
        );
        assert!(app.handle_news_tasks(now));
        let run = app.news.run.as_ref().expect("the first run started");
        assert_eq!(run.trigger, NewsTrigger::Scheduled);
        assert!(!app.news.first_run_pending);

        // The run ends without a record reaching the log yet: nothing
        // starts again (the rule is consumed, the slot not due).
        app.news.run = None;
        assert!(!app.handle_news_tasks(Instant::now()));
        assert!(app.news.run.is_none(), "not twice");
        // A reload that keeps news on owes nothing either.
        app.news.apply_config(&enabled_config());
        assert!(!app.news.first_run_pending);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A desk with a run record, or with a published edition, waits for its
    /// schedule after being enabled.
    #[tokio::test]
    async fn enabling_news_after_a_run_waits_for_the_schedule() {
        let ran = temp_home("first-run-ran");
        std::fs::create_dir_all(ran.join("runs")).unwrap();
        std::fs::write(
            store::index_path(&ran),
            "{\"started\":\"2026-09-21T09:00:00+00:00\",\"trigger\":\"manual\",\"outcome\":\"failed\"}\n",
        )
        .unwrap();
        let mut app = first_run_app(Some(ran.clone()));
        app.news.apply_config(&enabled_config());
        assert!(!app.handle_news_tasks(Instant::now()));
        assert!(app.news.run.is_none(), "a failed run counts as a run");
        assert!(!app.news.first_run_pending, "checked once");

        let home = temp_home("first-run-edition");
        editions_index(&home, &[1]);
        let mut app = first_run_app(Some(home.clone()));
        app.news.apply_config(&enabled_config());
        assert!(!app.handle_news_tasks(Instant::now()));
        assert!(app.news.run.is_none(), "an edition counts as a run");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&ran);
    }

    /// A run in flight keeps the first run owed until it ends; its record
    /// then makes the desk one that ran.
    #[test]
    fn a_run_in_flight_holds_the_first_run_and_its_record_ends_it() {
        let home = temp_home("first-run-inflight");
        let mut app = first_run_app(Some(home.clone()));
        app.news.apply_config(&enabled_config());
        let now = Instant::now();
        app.news.run = Some(in_flight(NOW, now));
        assert!(!app.handle_news_tasks(now - Duration::from_secs(1)));
        assert!(app.news.first_run_pending, "still owed while in flight");
        assert_eq!(app.news.run.as_ref().unwrap().started_at, NOW);

        std::fs::create_dir_all(home.join("runs")).unwrap();
        std::fs::write(
            store::index_path(&home),
            "{\"started\":\"2026-09-21T09:00:00+00:00\",\"trigger\":\"manual\",\"outcome\":\"ok\"}\n",
        )
        .unwrap();
        app.news.run = None;
        assert!(!app.handle_news_tasks(Instant::now()));
        assert!(app.news.run.is_none(), "the desk ran meanwhile");
        assert!(!app.news.first_run_pending);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Turning news off drops an owed first run; a failed first start stands
    /// for it like a slot's (no retry every tick).
    #[test]
    fn switching_off_drops_the_first_run_and_a_failed_start_is_not_retried() {
        let home = temp_home("first-run-off");
        let mut app = first_run_app(Some(home.clone()));
        app.news.apply_config(&enabled_config());
        app.news.apply_config(&NewsConfig::default());
        assert!(!app.news.first_run_pending);
        assert!(!app.handle_news_tasks(Instant::now()));
        assert!(app.news.run.is_none());
        let _ = std::fs::remove_dir_all(&home);

        let dir = temp_home("first-run-fails");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let mut app = first_run_app(Some(blocker.join("news")));
        app.news.apply_config(&enabled_config());
        assert!(!app.handle_news_tasks(Instant::now()), "nothing started");
        assert!(!app.news.first_run_pending);
        let attempted = app.news.last_started_at.expect("the attempt is remembered");
        assert!(attempted.abs_diff(unix_now()) <= 2, "{attempted}");
        assert!(!app.handle_news_tasks(Instant::now()));
        assert!(app.news.run.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `news.set_enabled` (a config write and reload) turns news on and
    /// the next pass starts the first run.
    #[tokio::test]
    async fn set_enabled_on_a_desk_that_never_ran_starts_the_first_run() {
        let dir = temp_home("first-run-reload");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[news]\nenabled = false\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = first_run_app(Some(dir.join("news")));
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsSetEnabled(crate::api::schema::NewsSetEnabledParams {
                enabled: true,
            }),
        );
        assert_eq!(response["result"]["news"]["enabled"], true, "{response}");
        assert!(app.news.first_run_pending, "the reload flipped it on");
        assert!(app.handle_news_tasks(Instant::now()));
        assert!(app.news.run.is_some());
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(&dir);
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
        // Noon on the fixed clock: 13:00 is an hour away from whatever the
        // real clock says.
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        app.news.last_started_at = Some(unix_now() - 60);
        app.news.consecutive_failures = 1;
        app.news.run = Some(in_flight(NOW - 60, Instant::now()));
        let response = request(
            &mut app,
            crate::api::schema::Method::NewsStatus(Default::default()),
        );
        let status = &response["result"]["status"];
        assert_eq!(response["result"]["type"], "news_status");
        assert_eq!(status["enabled"], true);
        assert_eq!(
            status["times"],
            serde_json::json!(["08:00", "13:00", "19:00"])
        );
        assert_eq!(status["quiet_hours"], "00:00-08:00");
        assert_eq!(status["home"], home.display().to_string());
        let next = status["next_run_at"].as_u64().expect("next_run_at");
        assert!(
            next.abs_diff(unix_now() + 3600) <= 2,
            "next run is the 13:00 slot: {next}"
        );
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
        assert_eq!(
            WATCHDOG,
            Duration::from_secs(70 * 60),
            "the budget plus 10 min"
        );
        // Past the watchdog the run goes stopping (no pane: nothing to
        // signal) and stays in flight; the missing pane then counts as a
        // shell prompt, and the second such poll records the timeout.
        assert!(app.handle_news_tasks(now + WATCHDOG));
        let run = app.news.run.as_ref().expect("still in flight");
        assert!(
            matches!(
                run.phase,
                NewsPhase::Stopping {
                    second_interrupt: None,
                    ..
                }
            ),
            "{:?}",
            run.phase
        );
        assert_eq!(run.info().phase, "stopping");
        assert_eq!(
            app.news.next_deadline(now + WATCHDOG, NOW, None),
            Some(now + WATCHDOG + POLL_INTERVAL),
            "the watchdog itself is not a deadline any more"
        );
        assert!(!app.handle_news_tasks(now + WATCHDOG + Duration::from_secs(1)));
        assert!(!app.handle_news_tasks(now + WATCHDOG + POLL_INTERVAL));
        assert!(
            app.news.run.is_some(),
            "one poll at the (missing) shell is not enough"
        );
        assert!(app.handle_news_tasks(now + WATCHDOG + 2 * POLL_INTERVAL));
        assert!(app.news.run.is_none());
        assert_eq!(app.news.consecutive_failures, 1);
        let history = store::read_history(&home, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "timeout");
        assert_eq!(history[0].started, iso_utc(NOW));
        assert_eq!(history[0].errors, ["no result after 70 minutes"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn the_watchdog_falls_back_to_two_ctrl_c_a_second_apart_and_waits_for_the_runner() {
        let home = temp_home("watchdog-ctrl-c");
        let mut app = news_app(Some(home.clone()), true);
        std::fs::create_dir_all(home.join("runs")).unwrap();
        let mut rx = news_tab_with_input(&mut app);
        // The test runtime has no child pid, so no process group is found
        // and the watchdog falls back to Ctrl-C; the pane is busy (the
        // runner) throughout.
        app.news.assume_shell_busy = true;
        let now = Instant::now();
        app.news.run = Some(in_flight(NOW, now));
        assert!(!app.handle_news_tasks(now));
        assert_eq!(typed(&mut rx), "");

        let fired = now + WATCHDOG;
        assert!(app.handle_news_tasks(fired));
        assert_eq!(typed(&mut rx), "\x03", "one Ctrl-C in its own write");
        let run = app.news.run.clone().expect("the run stays in flight");
        assert_eq!(
            run.phase,
            NewsPhase::Stopping {
                next_poll: fired + POLL_INTERVAL,
                second_interrupt: Some(fired + INTERRUPT_GAP),
            }
        );
        assert_eq!(
            app.news.next_deadline(fired, NOW, None),
            Some(fired + INTERRUPT_GAP),
            "the loop wakes for the second Ctrl-C"
        );
        assert!(!app.handle_news_tasks(fired + Duration::from_millis(500)));
        assert_eq!(typed(&mut rx), "", "not before the gap");
        assert!(app.handle_news_tasks(fired + INTERRUPT_GAP));
        assert_eq!(
            typed(&mut rx),
            "\x03",
            "the second Ctrl-C, a separate write"
        );
        assert!(app.news.run.is_some());
        assert!(!app.handle_news_tasks(fired + 2 * INTERRUPT_GAP));
        assert_eq!(typed(&mut rx), "", "no third one");

        // The runner took the interrupt and wrote its own record: that is
        // the run's outcome, not a server timeout.
        store::append_index_record(&home, &record(&iso_utc(NOW + 1), "interrupted")).unwrap();
        assert!(app.handle_news_tasks(fired + POLL_INTERVAL));
        assert!(app.news.run.is_none());
        let history = store::read_history(&home, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "interrupted");

        // No record but the pane back at its prompt: two polls, then the
        // server's timeout record.
        let mut run = in_flight(NOW + 100, now);
        run.index_len = store::index_len(&home);
        app.news.run = Some(run);
        assert!(app.handle_news_tasks(fired));
        assert_eq!(typed(&mut rx), "\x03");
        assert!(app.handle_news_tasks(fired + INTERRUPT_GAP));
        assert_eq!(typed(&mut rx), "\x03");
        app.news.assume_shell_busy = false;
        app.news.assume_shell_ready = true;
        assert!(!app.handle_news_tasks(fired + POLL_INTERVAL));
        assert!(app.news.run.is_some());
        assert!(app.handle_news_tasks(fired + 2 * POLL_INTERVAL));
        assert!(app.news.run.is_none());
        let history = store::read_history(&home, 10);
        assert_eq!(history[0].outcome, "timeout");
        assert_eq!(history[0].started, iso_utc(NOW + 100));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_run_whose_news_tab_is_gone_is_recorded_interrupted() {
        let home = temp_home("gone-tab");
        let mut app = news_app(Some(home.clone()), true);
        app.news.tab_id = Some("w_9:t_9".into());
        let now = Instant::now();
        app.news.run = Some(in_flight(NOW, now));
        assert!(!app.handle_news_tasks(now));
        assert!(
            app.news.run.is_some(),
            "one poll without the tab is not enough"
        );
        assert!(app.handle_news_tasks(now + POLL_INTERVAL));
        assert!(app.news.run.is_none(), "the second ends the run");
        let history = store::read_history(&home, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "interrupted");
        assert_eq!(app.news.consecutive_failures, 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_past_notify_retry_is_not_a_deadline() {
        let home = temp_home("retry-deadline");
        let mut app = news_app(Some(home.clone()), false);
        let now = Instant::now();
        assert_eq!(app.news.next_deadline(now, NOW, None), None);
        app.news.notify_retry_at = Some(now - Duration::from_secs(1));
        assert_eq!(
            app.news.next_deadline(now, NOW, None),
            None,
            "a past retry would spin the loop"
        );
        app.news.notify_retry_at = Some(now);
        assert_eq!(app.news.next_deadline(now, NOW, None), None);
        app.news.notify_retry_at = Some(now + Duration::from_secs(1));
        assert_eq!(
            app.news.next_deadline(now, NOW, None),
            Some(now + Duration::from_secs(1))
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn deadlines_follow_the_run_and_the_schedule() {
        let home = temp_home("deadline");
        let mut app = news_app(Some(home.clone()), true);
        let now = Instant::now();
        let noon = clock(12, 0, 0);
        app.news.last_started_at = Some(NOW - 60);
        assert_eq!(
            app.news.next_deadline(now, NOW, noon),
            Some(now + Duration::from_secs(3600)),
            "the 13:00 slot"
        );
        assert_eq!(
            app.news
                .next_deadline(now, at(13, 0) + 1_000, clock(13, 16, 40)),
            Some(now),
            "a slot in the past without a run since is due now"
        );
        assert_eq!(
            app.news.next_deadline(now, NOW, None),
            None,
            "no local clock: no slot to wake for"
        );
        app.news.times.clear();
        assert_eq!(app.news.next_deadline(now, NOW, noon), None, "no times");
        app.news.times = TIMES.to_vec();
        app.news.run = Some(in_flight(NOW, now));
        assert_eq!(app.news.next_deadline(now, NOW, noon), Some(now));
        let mut stopping = in_flight(NOW, now - WATCHDOG);
        stopping.phase = NewsPhase::Stopping {
            next_poll: now + POLL_INTERVAL,
            second_interrupt: Some(now + INTERRUPT_GAP),
        };
        app.news.run = Some(stopping);
        assert_eq!(
            app.news.next_deadline(now, NOW, noon),
            Some(now + INTERRUPT_GAP),
            "stopping: the second Ctrl-C, not the past watchdog"
        );
        app.news.run = None;
        app.news.enabled = false;
        assert_eq!(app.news.next_deadline(now, NOW, noon), None);
        let disabled_home = news_app(None, true);
        assert_eq!(disabled_home.news.next_deadline(now, NOW, noon), None);
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

    /// Edition files `n` with the given story urls (the lead first), plus
    /// their index entries.
    fn editions_with_stories(home: &Path, editions: &[(u32, &[&str])]) {
        for (n, urls) in editions {
            let path = home.join(format!("editions/2026-09-{n:02}/0900-e{n:04}.json"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let items = urls[1..]
                .iter()
                .map(|u| serde_json::json!({ "head": "H", "url": u }))
                .collect::<Vec<_>>();
            let page = serde_json::json!({
                "edition": n,
                "lead": { "head": "Lead", "url": urls[0] },
                "sections": [{ "title": "T", "items": items }]
            });
            std::fs::write(path, page.to_string()).unwrap();
        }
        let numbers = editions.iter().map(|(n, _)| *n).collect::<Vec<_>>();
        editions_index(home, &numbers);
    }

    #[test]
    fn the_last_read_edition_follows_the_focused_viewer_and_counts_new_stories() {
        let home = temp_home("last-read");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        editions_with_stories(
            &home,
            &[
                (1, &["https://x/lead", "https://x/a"]),
                (2, &["https://x/lead", "https://x/b", "https://x/c"]),
            ],
        );
        let store_path = home.join(store::FILE_NAME);
        let mut app = news_app(Some(home.clone()), false);
        app.news.store = Some(store_path.clone());
        app.state.workspaces[0].test_add_tab(Some("other"));
        news_tab_at(&mut app, 1);
        app.state.switch_workspace_tab(0, 1);
        app.news.assume_shell_busy = true;
        let now = Instant::now();
        assert!(
            !app.handle_news_tasks(now),
            "no viewer state yet: nothing read"
        );
        assert_eq!(app.news.last_read_edition, None);
        let info = app.news_get_info();
        assert_eq!(info.last_read_edition, None);
        assert_eq!(info.new_stories, None, "unknown until something was read");

        // The viewer shows edition 1 while the tab is focused and busy.
        std::fs::write(
            store::viewer_state_path(&home),
            r#"{"version":1,"showing":1,"at":"2026-09-01T07:00:00+00:00"}"#,
        )
        .unwrap();
        app.state.switch_workspace_tab(0, 0);
        assert!(!app.handle_news_tasks(now), "unfocused: not read");
        app.state.switch_workspace_tab(0, 1);
        app.news.assume_shell_busy = false;
        app.news.assume_shell_ready = true;
        assert!(
            !app.handle_news_tasks(now),
            "at a shell prompt: the viewer is not up"
        );
        app.news.assume_shell_ready = false;
        app.news.assume_shell_busy = true;
        assert!(app.handle_news_tasks(now), "focused and showing: read");
        assert_eq!(app.news.last_read_edition, Some(1));
        let read: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(store::read_record_path(&home)).unwrap())
                .unwrap();
        assert_eq!(read["last_read_edition"], 1, "mirrored for the viewer");
        assert_eq!(
            store::load(&store_path).last_read_edition,
            Some(1),
            "persisted"
        );
        let info = app.news_get_info();
        assert_eq!(info.last_read_edition, Some(1));
        assert_eq!(info.new_stories, Some(2), "b and c are new since edition 1");
        assert!(!app.handle_news_tasks(now), "and nothing changes after");

        // Opening an older edition never moves the mark backwards; the newest does.
        std::fs::write(
            store::viewer_state_path(&home),
            r#"{"version":1,"showing":2,"at":"2026-09-02T07:00:00+00:00"}"#,
        )
        .unwrap();
        assert!(app.handle_news_tasks(now));
        assert_eq!(app.news.last_read_edition, Some(2));
        assert_eq!(app.news_get_info().new_stories, Some(0));
        std::fs::write(
            store::viewer_state_path(&home),
            r#"{"version":1,"showing":1,"at":"2026-09-02T08:00:00+00:00"}"#,
        )
        .unwrap();
        assert!(!app.handle_news_tasks(now));
        assert_eq!(app.news.last_read_edition, Some(2));

        // A fresh server restores the mark from news.json.
        let mut restarted = news_app(Some(home.clone()), false);
        restarted.news.store = Some(store_path.clone());
        restarted.news.load_record(store::load(&store_path), now);
        assert_eq!(restarted.news.last_read_edition, Some(2));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_run_notification_leads_with_the_new_story_count() {
        let home = temp_home("new-body");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        editions_with_stories(
            &home,
            &[
                (1, &["https://x/lead", "https://x/a"]),
                (
                    2,
                    &[
                        "https://x/lead",
                        "https://x/b",
                        "https://x/c",
                        "https://x/d",
                    ],
                ),
            ],
        );
        let mut app = news_app(Some(home.clone()), false);
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        app.news.last_read_edition = Some(1);
        let now = Instant::now();
        complete(
            &mut app,
            &home,
            &notify_record(NOW, "Sonnet 5.5", "low"),
            now,
        );
        assert_eq!(app.news.notify.pending.len(), 1);
        assert_eq!(
            app.news.notify.pending[0].body.as_deref(),
            Some("3 new · Out now."),
            "the count leads the editor's body"
        );
        // Without a body only the count; nothing read yet: the plain body.
        let mut bare = notify_record(NOW + 10, "Bare", "low");
        bare.notify.as_mut().unwrap().body = None;
        complete(&mut app, &home, &bare, now);
        assert_eq!(
            app.news.notify.pending.len(),
            1,
            "a run notification replaces the last"
        );
        assert_eq!(app.news.notify.pending[0].body.as_deref(), Some("3 new"));
        app.news.last_read_edition = None;
        complete(
            &mut app,
            &home,
            &notify_record(NOW + 20, "Plain", "low"),
            now,
        );
        assert_eq!(app.news.notify.pending[0].body.as_deref(), Some("Out now."));
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

    /// The News tab (second tab) with a runtime whose input the test reads.
    fn news_tab_with_input(app: &mut App) -> tokio::sync::mpsc::Receiver<Bytes> {
        app.state.workspaces[0].test_add_tab(Some("other"));
        news_tab_at(app, 1);
        let tab = &app.state.workspaces[0].tabs[1];
        let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id, runtime);
        rx
    }

    fn typed(rx: &mut tokio::sync::mpsc::Receiver<Bytes>) -> String {
        let mut out = String::new();
        while let Ok(bytes) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    #[tokio::test]
    async fn a_news_tab_focused_at_a_bare_shell_shows_the_page_again() {
        let home = temp_home("refocus");
        let mut app = news_app(Some(home.clone()), false);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(store::page_path(&home), "{}").unwrap();
        let mut rx = news_tab_with_input(&mut app);
        app.news.assume_shell_ready = true;
        let now = Instant::now();

        app.state.switch_workspace_tab(0, 0);
        app.handle_news_tasks(now);
        assert_eq!(typed(&mut rx), "", "not focused: nothing typed");

        // Gaining focus at a shell (a restart, a quit viewer) brings the page back.
        app.state.switch_workspace_tab(0, 1);
        assert!(app.handle_news_tasks(now));
        let input = typed(&mut rx);
        assert!(
            input.contains("viewer.py") && input.ends_with('\r'),
            "{input:?}"
        );
        assert!(
            home.join("bin").join("viewer.py").is_file(),
            "the viewer is installed"
        );

        // Staying focused does not type it again.
        app.handle_news_tasks(now);
        assert_eq!(typed(&mut rx), "");

        // Busy (the viewer, or anything else): focus does nothing.
        app.state.switch_workspace_tab(0, 0);
        app.handle_news_tasks(now);
        app.news.assume_shell_busy = true;
        app.state.switch_workspace_tab(0, 1);
        app.handle_news_tasks(now);
        assert_eq!(typed(&mut rx), "", "a busy pane is left alone");

        // No page yet: nothing to show.
        app.news.assume_shell_busy = false;
        std::fs::remove_file(store::page_path(&home)).unwrap();
        app.state.switch_workspace_tab(0, 0);
        app.handle_news_tasks(now);
        app.state.switch_workspace_tab(0, 1);
        app.handle_news_tasks(now);
        assert_eq!(typed(&mut rx), "");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_run_whose_pane_is_back_at_its_shell_without_a_record_is_interrupted() {
        let home = temp_home("interrupted");
        let mut app = news_app(Some(home.clone()), false);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(store::page_path(&home), "{}").unwrap();
        let mut rx = news_tab_with_input(&mut app);
        app.state.switch_workspace_tab(0, 1);
        let now = Instant::now();
        app.news.assume_shell_busy = true;
        app.handle_news_tasks(now); // focus recorded while busy: no viewer
        app.news.run = Some(in_flight(NOW, now));

        // Busy (the runner is the foreground process): keep waiting.
        assert!(!app.handle_news_tasks(now));
        assert!(app.news.run.is_some());

        // Back at the shell with no record: one poll is not enough ...
        app.news.assume_shell_busy = false;
        app.news.assume_shell_ready = true;
        assert!(!app.handle_news_tasks(now + POLL_INTERVAL));
        assert!(
            app.news.run.is_some(),
            "one poll at the shell is not enough"
        );
        // ... the second one records the run as interrupted.
        assert!(app.handle_news_tasks(now + 2 * POLL_INTERVAL));
        assert!(app.news.run.is_none());
        let log = std::fs::read_to_string(store::index_path(&home)).unwrap();
        assert!(log.contains("\"interrupted\""), "{log}");
        assert_eq!(app.news.consecutive_failures, 1);

        // And the page comes back on the next pass.
        let _ = typed(&mut rx);
        app.handle_news_tasks(now + 2 * POLL_INTERVAL);
        assert!(typed(&mut rx).contains("viewer.py"));
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
        assert_eq!(
            rx.try_recv().unwrap().as_ref(),
            b"\x1b[9999~",
            "the pinned viewer is quit with herdr's private sequence"
        );
        let pending = app.news.pending_command.clone().expect("still pending");
        assert!(pending.quit_sent);
        assert_eq!(pending.next_check, now + START_RETRY);
        assert_eq!(
            app.news.next_deadline(now, NOW, None),
            Some(now + START_RETRY),
            "the loop wakes for the probe"
        );
        app.handle_news_tasks(now + START_RETRY);
        assert!(rx.try_recv().is_err(), "q is sent once");
        // The prompt is back.
        app.news.assume_shell_busy = false;
        app.handle_news_tasks(now + 2 * START_RETRY);
        assert_eq!(
            rx.try_recv().unwrap().as_ref(),
            b"\x15echo hi\r",
            "kill-line first, so a half-typed prompt line is not prepended"
        );
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
    fn set_quiet_hours_writes_the_canonical_window_and_applies_it() {
        let dir = temp_home("set-quiet");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[news]\nenabled = true\ntimes = [\"09:00\"]\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = news_app(Some(dir.join("news")), true);
        let set = |app: &mut App, window: &str| {
            request(
                app,
                crate::api::schema::Method::NewsSetQuietHours(
                    crate::api::schema::NewsSetQuietHoursParams {
                        quiet_hours: window.into(),
                    },
                ),
            )
        };
        let response = set(&mut app, " 22:00 - 7:00 ");
        assert_eq!(response["result"]["type"], "news_get", "{response}");
        assert_eq!(
            response["result"]["news"]["quiet_hours"], "22:00-07:00",
            "canonical"
        );
        assert_eq!(app.news.quiet_text, "22:00-07:00");
        assert_eq!(
            app.news.quiet.map(|q| (q.start, q.end)),
            Some((22 * 60, 7 * 60))
        );
        let written: crate::config::Config =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written.news.quiet_hours, "22:00-07:00");
        assert_eq!(written.news.times, ["09:00"], "other keys are kept");
        let response = set(&mut app, "");
        assert_eq!(response["result"]["news"]["quiet_hours"], "");
        assert!(app.news.quiet.is_none());
        let response = set(&mut app, "25:00-08:00");
        assert_eq!(
            response["error"]["code"], "news_invalid_quiet_hours",
            "{response}"
        );
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("expected HH:MM-HH:MM"));
        assert!(app.news.quiet.is_none(), "a refused window changes nothing");
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_the_reader_cancelled_does_not_count_toward_the_failure_alert() {
        let home = temp_home("user-cancel");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        let mut app = news_app(Some(home.clone()), false);
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        let now = Instant::now();
        let interrupted = |n: u64| NewsRunRecord {
            started: iso_utc(NOW + n),
            trigger: "manual".into(),
            outcome: "interrupted".into(),
            errors: vec!["interrupted by the user".into()],
            ..NewsRunRecord::default()
        };
        let invalid = NewsRunRecord {
            started: iso_utc(NOW + 10),
            trigger: "scheduled".into(),
            outcome: "invalid".into(),
            errors: vec!["lead text ends in an ellipsis".into()],
            ..NewsRunRecord::default()
        };
        complete(&mut app, &home, &invalid, now);
        assert_eq!(app.news.consecutive_failures, 1);
        // the reader's double Ctrl-C: the runner's own `interrupted`
        complete(&mut app, &home, &interrupted(1), now);
        assert_eq!(
            app.news.consecutive_failures, 1,
            "a user cancel neither counts nor resets"
        );
        complete(&mut app, &home, &interrupted(2), now);
        complete(&mut app, &home, &interrupted(3), now);
        assert_eq!(app.news.consecutive_failures, 1);
        assert!(
            app.news.notify.pending.is_empty(),
            "no failure alert from cancels"
        );
        // the same runner record during the watchdog's Stopping phase is a real failure
        let mut run = in_flight(NOW + 4, now);
        run.index_len = store::index_len(&home);
        run.phase = NewsPhase::Stopping {
            next_poll: now,
            second_interrupt: None,
        };
        app.news.run = Some(run);
        store::append_index_record(&home, &interrupted(4)).unwrap();
        assert!(app.handle_news_tasks(now + POLL_INTERVAL));
        assert!(app.news.run.is_none());
        assert_eq!(app.news.consecutive_failures, 2, "a watchdog stop counts");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn set_enabled_writes_the_config_and_applies_it() {
        let dir = temp_home("set-enabled");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[news]\nenabled = false\ntimes = [\"09:00\"]\n").unwrap();
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
        assert_eq!(written.news.times, ["09:00"], "other keys are kept");

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
    fn set_times_writes_the_canonical_list_and_applies_it() {
        let dir = temp_home("set-times");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[news]\nenabled = true\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = news_app(Some(dir.join("news")), true);
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        app.news.last_started_at = Some(unix_now() - 60);
        let set = |app: &mut App, times: &[&str]| {
            request(
                app,
                crate::api::schema::Method::NewsSetTimes(crate::api::schema::NewsSetTimesParams {
                    times: times.iter().map(|time| (*time).to_string()).collect(),
                }),
            )
        };
        let response = set(&mut app, &["19:00", "8:00", "19:00", "14:30"]);
        assert_eq!(response["result"]["type"], "news_get", "{response}");
        assert_eq!(
            response["result"]["news"]["times"],
            serde_json::json!(["08:00", "14:30", "19:00"]),
            "sorted, zero-padded, without duplicates"
        );
        assert_eq!(app.news.times, [8 * 60, 14 * 60 + 30, 19 * 60]);
        let written: crate::config::Config =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written.news.times, ["08:00", "14:30", "19:00"]);
        assert!(written.news.enabled, "other keys are kept");
        let next = response["result"]["news"]["next_run_at"]
            .as_u64()
            .expect("next_run_at");
        assert!(
            next.abs_diff(unix_now() + 2 * 3600 + 30 * 60) <= 2,
            "the next run follows the new list at once: {next}"
        );

        let response = set(&mut app, &["08:00", "25:00"]);
        assert_eq!(response["error"]["code"], "news_invalid_time", "{response}");
        assert_eq!(app.news.times, [8 * 60, 14 * 60 + 30, 19 * 60], "unchanged");

        let response = set(&mut app, &[]);
        assert_eq!(response["result"]["news"]["times"], serde_json::json!([]));
        assert!(
            response["result"]["news"].get("next_run_at").is_none(),
            "no times, no next run: {response}"
        );
        assert!(app.news.times.is_empty());
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notify_policy_allows_two_a_day_and_one_high_urgency_extra() {
        let mut ledger = store::NewsNotifyRecord::default();
        let day = "2026-09-29";
        assert_eq!(
            notify_decision(&mut ledger, day, false),
            NotifyDecision::Deliver
        );
        assert_eq!(
            notify_decision(&mut ledger, day, false),
            NotifyDecision::Deliver
        );
        assert!(matches!(
            notify_decision(&mut ledger, day, false),
            NotifyDecision::Drop(_)
        ));
        assert_eq!(
            notify_decision(&mut ledger, day, true),
            NotifyDecision::Deliver,
            "high urgency passes the cap once"
        );
        assert!(matches!(
            notify_decision(&mut ledger, day, true),
            NotifyDecision::Drop(_)
        ));
        assert_eq!(ledger.delivered, 3);
        assert_eq!(
            notify_decision(&mut ledger, "2026-09-30", false),
            NotifyDecision::Deliver,
            "a new day starts over"
        );
        assert_eq!(ledger.delivered, 1);
        assert!(!ledger.high_extra_used);
        // A high one counts against the plain cap first.
        let mut fresh = store::NewsNotifyRecord::default();
        notify_decision(&mut fresh, day, true);
        assert!(!fresh.high_extra_used);
    }

    fn notify_record(started: u64, title: &str, urgency: &str) -> NewsRunRecord {
        NewsRunRecord {
            started: iso_utc(started),
            ended: Some(iso_utc(started + 300)),
            trigger: "scheduled".into(),
            outcome: "ok".into(),
            edition: Some(2),
            changed: true,
            notify: Some(crate::api::schema::NewsNotifyInfo {
                title: title.into(),
                body: Some(" Out now. ".into()),
                urgency: urgency.into(),
            }),
            ..NewsRunRecord::default()
        }
    }

    /// Complete a run in flight with `record` through the watcher.
    fn complete(app: &mut App, home: &Path, record: &NewsRunRecord, now: Instant) {
        let mut run = in_flight(iso_to_unix_or_zero(&record.started), now);
        run.index_len = store::index_len(home);
        app.news.run = Some(run);
        store::append_index_record(home, record).unwrap();
        assert!(app.handle_news_tasks(now + POLL_INTERVAL));
        assert!(app.news.run.is_none());
    }

    fn iso_to_unix_or_zero(iso: &str) -> u64 {
        store::iso_to_unix(iso).unwrap_or(0)
    }

    #[test]
    fn a_changed_run_with_a_notify_request_queues_one_notification_without_charging_the_cap() {
        let home = temp_home("notify");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        let mut app = news_app(Some(home.clone()), true);
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        app.state.workspaces[0].tabs[0].set_custom_name(NEWS_TAB_LABEL.into());
        app.news.tab_id = app.public_tab_id(0, 0);
        app.state.switch_workspace_tab(0, 0);
        let now = Instant::now();

        // Not changed, or no request: nothing queued.
        let mut quiet_run = notify_record(NOW, "Nothing new", "low");
        quiet_run.changed = false;
        complete(&mut app, &home, &quiet_run, now);
        let mut silent = notify_record(NOW + 10, "No request", "low");
        silent.notify = None;
        complete(&mut app, &home, &silent, now);
        assert!(app.news.notify.pending.is_empty());

        complete(
            &mut app,
            &home,
            &notify_record(NOW + 20, " Sonnet 5.5 ", "low"),
            now,
        );
        assert_eq!(app.news.notify.pending.len(), 1);
        let pending = &app.news.notify.pending[0];
        assert_eq!(pending.kind, "run");
        assert_eq!(pending.title, "News: Sonnet 5.5");
        assert_eq!(pending.body.as_deref(), Some("Out now."));
        assert!(!pending.high);
        assert_eq!(pending.deliver_after, 0, "noon: no quiet hours");
        assert_eq!(app.news.due_notification_index(NOW), Some(0));
        assert_eq!(app.news.status().pending_notifications, 1);
        assert_eq!(app.news_get_info().pending_notifications, 1);
        assert_eq!(
            app.news.notify.delivered, 0,
            "the cap is charged at delivery"
        );

        // The second replaces the undelivered first (one queued per kind)
        // and still no slot is used: the replaced one never went out.
        complete(
            &mut app,
            &home,
            &notify_record(NOW + 30, "GPT-6", "low"),
            now,
        );
        assert_eq!(app.news.notify.pending.len(), 1);
        assert_eq!(app.news.notify.pending[0].title, "News: GPT-6");
        assert_eq!(app.news.notify.delivered, 0);
        assert!(app.news.notify.day.is_empty());

        // A high one is queued as such; the server decides at delivery.
        complete(
            &mut app,
            &home,
            &notify_record(NOW + 50, "Urgent", "high"),
            now,
        );
        assert_eq!(app.news.notify.pending.len(), 1);
        assert!(app.news.notify.pending[0].high);

        // Delivery is the server's; a queued notification is due at once,
        // so the loop's deadline is the schedule's (a slot owed at noon).
        assert!(app.news.next_deadline(now, NOW, clock(12, 0, 0)).is_some());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn notification_text_is_sanitized_when_queued() {
        let home = temp_home("notify-sanitize");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        let mut app = news_app(Some(home.clone()), false);
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        let now = Instant::now();

        let long = "x".repeat(300);
        let mut record = notify_record(
            NOW,
            &format!("A\x07 title\n\twith\x1b[31m ctl {long}"),
            "low",
        );
        record.notify.as_mut().unwrap().body = Some(format!("body\x1b]8;;evil\x07 {long}"));
        complete(&mut app, &home, &record, now);
        let pending = app.news.notify.pending[0].clone();
        assert!(
            pending.title.starts_with("News: A title with[31m ctl xxx"),
            "{}",
            pending.title
        );
        assert_eq!(pending.title.chars().count(), 80);
        let body = pending.body.expect("a body");
        assert!(body.starts_with("body]8;;evil xxx"), "{body}");
        assert_eq!(body.chars().count(), 240);
        assert!(!body.chars().any(char::is_control));

        // Nothing left of the title once sanitized: no notification.
        app.news.notify.pending.clear();
        complete(
            &mut app,
            &home,
            &notify_record(NOW + 10, "\x07\x1b \t", "high"),
            now,
        );
        assert!(
            app.news.notify.pending.is_empty(),
            "an empty title is dropped"
        );

        // The failure alert's body carries runner text: sanitized too.
        let failed = |n: u64| NewsRunRecord {
            started: iso_utc(NOW + 100 + n),
            trigger: "scheduled".into(),
            outcome: "invalid".into(),
            errors: vec![format!("boom\x1b[2J\x07 {long}")],
            ..NewsRunRecord::default()
        };
        for n in 0..3 {
            complete(&mut app, &home, &failed(n), now);
        }
        let alert = app
            .news
            .notify
            .pending
            .iter()
            .find(|p| p.kind == "failures")
            .unwrap();
        let body = alert.body.clone().unwrap();
        assert!(body.starts_with("3 in a row · boom[2J xxx"), "{body}");
        assert_eq!(body.chars().count(), 240);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_starting_run_resends_the_viewer_quit_on_every_retry_and_types_after_kill_line() {
        let home = temp_home("start-retry");
        let mut app = news_app(Some(home.clone()), false);
        let mut rx = news_tab_with_input(&mut app);
        app.news.assume_shell_busy = true;
        let now = Instant::now();
        app.start_news_run(NewsTrigger::Manual, now).unwrap();
        assert_eq!(
            typed(&mut rx),
            "\x1b[9999~",
            "busy pane: the viewer is quit"
        );
        assert!(app.handle_news_tasks(now + START_RETRY));
        assert_eq!(
            typed(&mut rx),
            "\x1b[9999~",
            "still busy: quit again (a split or lost sequence must not cost the start)"
        );
        app.news.assume_shell_busy = false;
        app.news.assume_shell_ready = true;
        assert!(app.handle_news_tasks(now + 2 * START_RETRY));
        let input = typed(&mut rx);
        assert!(input.starts_with("\x15python3 "), "{input:?}");
        assert!(
            input.contains("--deadline-min 60 --pinned") && input.ends_with('\r'),
            "{input:?}"
        );
        assert!(matches!(
            app.news.run.as_ref().unwrap().phase,
            NewsPhase::Running { .. }
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn quiet_hours_hold_the_notification_until_they_end() {
        let home = temp_home("notify-quiet");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        let mut app = news_app(Some(home.clone()), false);
        app.news.local_override = Some((clock(3, 30, 15).unwrap(), "2026-09-29"));
        let now = Instant::now();
        complete(&mut app, &home, &notify_record(NOW, "Night", "low"), now);
        let now_unix = unix_now();
        let pending = &app.news.notify.pending[0];
        let expected = now_unix + (4 * 60 + 30) * 60 - 15;
        assert!(
            pending.deliver_after.abs_diff(expected) <= 2,
            "deliver at 08:00: {} vs {expected}",
            pending.deliver_after
        );
        assert!(app.news.due_notification_index(now_unix).is_none());
        assert_eq!(
            app.news.due_notification_index(pending.deliver_after),
            Some(0)
        );
        let deadline = app
            .news
            .next_deadline(now, now_unix, None)
            .expect("the loop wakes for the end of quiet hours, even with scheduling off");
        let wait = deadline.saturating_duration_since(now).as_secs();
        assert!(wait.abs_diff((4 * 60 + 30) * 60 - 15) <= 2, "{wait}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn three_failed_runs_raise_one_alert_until_a_success() {
        let home = temp_home("notify-failures");
        std::fs::create_dir_all(home.join("runs")).unwrap();
        let mut app = news_app(Some(home.clone()), false);
        app.news.local_override = Some((clock(12, 0, 0).unwrap(), "2026-09-29"));
        let now = Instant::now();
        let failed = |n: u64| NewsRunRecord {
            started: iso_utc(NOW + n),
            trigger: "scheduled".into(),
            outcome: "invalid".into(),
            errors: vec![format!("lead text ends in an ellipsis ({n})")],
            ..NewsRunRecord::default()
        };
        complete(&mut app, &home, &failed(1), now);
        complete(&mut app, &home, &failed(2), now);
        assert!(
            app.news.notify.pending.is_empty(),
            "two failures: nothing yet"
        );
        complete(&mut app, &home, &failed(3), now);
        assert_eq!(app.news.consecutive_failures, 3);
        assert_eq!(app.news.notify.pending.len(), 1);
        let alert = &app.news.notify.pending[0];
        assert_eq!(alert.kind, "failures");
        assert_eq!(alert.title, "News runs failing");
        assert_eq!(
            alert.body.as_deref(),
            Some("3 in a row · lead text ends in an ellipsis (3)")
        );
        assert!(app.news.notify.failure_alerted);
        complete(&mut app, &home, &failed(4), now);
        assert_eq!(app.news.notify.pending.len(), 1, "not repeated");
        assert_eq!(
            app.news.notify.pending[0].body.as_deref(),
            Some("3 in a row · lead text ends in an ellipsis (3)")
        );

        app.news.notify.pending.clear();
        let mut ok = notify_record(NOW + 5, "Back", "low");
        ok.notify = None;
        complete(&mut app, &home, &ok, now);
        assert_eq!(app.news.consecutive_failures, 0);
        assert!(!app.news.notify.failure_alerted);
        for n in 6..9 {
            complete(&mut app, &home, &failed(n), now);
        }
        assert_eq!(
            app.news.notify.pending.len(),
            1,
            "a new streak alerts again"
        );
        assert_eq!(
            app.news.notify.pending[0].body.as_deref(),
            Some("3 in a row · lead text ends in an ellipsis (8)")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_persisted_run_is_watched_again_after_a_restart() {
        let dir = temp_home("restart");
        let store_path = dir.join(store::FILE_NAME);
        store::save(
            &store_path,
            &store::NewsRecord {
                last_read_edition: None,
                last_started_at: Some(NOW),
                tab_id: Some("w_1:t_1".into()),
                pane_id: None,
                run: Some(store::PersistedNewsRun {
                    started_at: NOW,
                    started: iso_utc(NOW),
                    trigger: "scheduled".into(),
                    index_len: 7,
                }),
                consecutive_failures: 3,
                notify: store::NewsNotifyRecord {
                    day: "2026-09-29".into(),
                    delivered: 2,
                    high_extra_used: true,
                    failure_alerted: true,
                    pending: vec![store::PendingNewsNotify {
                        kind: "run".into(),
                        title: "News: t".into(),
                        body: None,
                        high: false,
                        deliver_after: NOW + 60,
                        queued_at: NOW,
                    }],
                },
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
        assert_eq!(state.last_started_at, Some(NOW));
        assert_eq!(state.tab_id.as_deref(), Some("w_1:t_1"));
        assert_eq!(state.notify.delivered, 2);
        assert!(state.notify.failure_alerted);
        assert_eq!(state.notify.pending.len(), 1);
        assert_eq!(state.due_notification_index(NOW), None);
        assert_eq!(state.due_notification_index(NOW + 60), Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
