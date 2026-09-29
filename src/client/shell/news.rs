//! The News tab on the client (fork): the client-only state behind the
//! `tabs` sidebar's pinned News row and the settings' news section.
//!
//! Everything shown comes from `news.get`, pulled on the tick after the
//! first snapshot of a connection, whenever the tab set or the News tab's
//! status or mark changes in the snapshot, after every `news.*` reply, and
//! otherwise once a minute (a `herdr news enable` from a shell changes no
//! snapshot). Nothing is pulled from a server that does not advertise
//! `news.get`.
//!
//! The row (`news_row`) exists only while `news.get` named a tab and the
//! active snapshot still lists it: a state glyph, `News`, and a short
//! status, in priority order `running 2m` (a run in flight), `unread` (the
//! tab is important), `failed` (the last run did not succeed), `paused`
//! (scheduling off), else the last run's local time.

use super::*;
use crate::api::schema::{EmptyParams, Method, NewsGetInfo, NewsOpenParams, NewsSetEnabledParams};

/// How often the record is pulled without a visible reason.
pub(super) const NEWS_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Client-only news state for the active endpoint.
#[derive(Debug, Default)]
pub(crate) struct ClientNewsState {
    /// The last `news.get` reply; `None` before one, or after an error.
    pub(super) info: Option<NewsGetInfo>,
    /// A `news.get` is in flight.
    pub(super) loading: bool,
    /// Pull on the next tick (a `news.*` reply, a settings section opening).
    pub(super) refresh_due: bool,
    /// The snapshot facts the last pull was made for; a change pulls again.
    pub(super) pulled_for: Option<NewsSnapshotSignature>,
    /// The `running Xm` minute drawn last, to repaint on the next minute.
    pub(super) drawn_running_minutes: Option<u64>,
    /// When the last pull was sent, for the periodic refresh.
    pub(super) last_pull: Option<std::time::Instant>,
}

/// What of the snapshot decides a fresh `news.get`: the connection, the
/// tab ids (the News tab appearing or going), and the News tab's status
/// and mark (a run starting or finishing, the unread mark clearing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NewsSnapshotSignature {
    boot_id: String,
    tab_ids: Vec<String>,
    news_tab: Option<(crate::api::schema::AgentStatus, bool)>,
}

/// The pinned row's state, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewsRowState {
    Running,
    Unread,
    Failed,
    Paused,
    Idle,
}

impl NewsRowState {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Running => "◐",
            Self::Unread => "●",
            Self::Failed => "×",
            Self::Paused => "◌",
            Self::Idle => "○",
        }
    }

    pub(crate) fn color(self, palette: &Palette) -> ratatui::style::Color {
        match self {
            Self::Running => palette.yellow,
            Self::Unread => palette.accent,
            Self::Failed => palette.red,
            Self::Paused | Self::Idle => palette.overlay0,
        }
    }
}

/// The pinned row, as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewsRow {
    pub(crate) tab_id: String,
    pub(crate) state: NewsRowState,
    /// `running 2m`, `unread`, `failed`, `paused` or the last run's `HH:MM`.
    pub(crate) status: String,
    pub(crate) focused: bool,
}

/// The row's label.
pub(crate) const NEWS_ROW_LABEL: &str = "News";

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// `HH:MM` local for a Unix time (the UTC offset in effect now).
pub(super) fn local_hhmm(unix: u64) -> String {
    let offset_seconds = crate::platform::local_datetime()
        .map(|local| {
            let utc = time::OffsetDateTime::now_utc();
            let utc = time::PrimitiveDateTime::new(utc.date(), utc.time());
            ((local - utc).whole_seconds() + 30).div_euclid(60) * 60
        })
        .unwrap_or(0);
    let shifted = i64::try_from(unix).unwrap_or(0) + offset_seconds;
    let date = time::OffsetDateTime::from_unix_timestamp(shifted)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:02}:{:02}", date.hour(), date.minute())
}

/// `2m`, `1h05m` for a run that started `started_at`.
pub(super) fn running_for(started_at: u64, now: u64) -> String {
    let minutes = now.saturating_sub(started_at) / 60;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h{:02}m", minutes / 60, minutes % 60)
    }
}

/// The row's state and status text for `info` and the News tab's snapshot
/// row (`important` is read from the snapshot: the server clears it on
/// focus before the next `news.get`).
pub(crate) fn news_row_state(
    info: &NewsGetInfo,
    important: bool,
    now: u64,
) -> (NewsRowState, String) {
    if let Some(run) = &info.run {
        return (
            NewsRowState::Running,
            format!("running {}", running_for(run.started_at, now)),
        );
    }
    if important {
        return (NewsRowState::Unread, "unread".into());
    }
    if info
        .last_run
        .as_ref()
        .is_some_and(|last| !matches!(last.outcome.as_str(), "ok" | "dry-run"))
    {
        return (NewsRowState::Failed, "failed".into());
    }
    if !info.enabled {
        // Paused: say how long ago the last page was written, if ever.
        return match info.last_run.as_ref() {
            Some(last) => (
                NewsRowState::Paused,
                format!("{} ago", ago(last.ended_at.unwrap_or(last.started_at), now)),
            ),
            None => (NewsRowState::Paused, "paused".into()),
        };
    }
    // Scheduled: the useful fact is when the next page comes.
    let status = match info.next_run_at {
        Some(next) if next > now => format!("next {}", local_hhmm(next)),
        Some(_) => "due".into(),
        None => "—".into(),
    };
    (NewsRowState::Idle, status)
}

/// A compact age: `now`, `12m`, `3h`, `2d`.
pub(super) fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

impl ClientShellState {
    /// The pinned row, while `news.get` named a tab the active snapshot
    /// still lists.
    pub(crate) fn news_row(&self) -> Option<NewsRow> {
        let info = self.news.info.as_ref()?;
        let tab_id = info.tab_id.as_deref()?;
        let tab = self
            .snapshot
            .as_deref()?
            .tabs
            .iter()
            .find(|tab| tab.tab_id == tab_id)?;
        let (state, status) = news_row_state(info, tab.important, unix_now());
        Some(NewsRow {
            tab_id: tab.tab_id.clone(),
            state,
            status,
            focused: tab.focused,
        })
    }

    fn news_snapshot_signature(&self) -> Option<NewsSnapshotSignature> {
        let snapshot = self.snapshot.as_deref()?;
        let news_tab_id = self
            .news
            .info
            .as_ref()
            .and_then(|info| info.tab_id.as_deref());
        Some(NewsSnapshotSignature {
            boot_id: snapshot.boot_id.clone(),
            tab_ids: snapshot.tabs.iter().map(|tab| tab.tab_id.clone()).collect(),
            news_tab: news_tab_id.and_then(|tab_id| {
                snapshot
                    .tabs
                    .iter()
                    .find(|tab| tab.tab_id == tab_id)
                    .map(|tab| (tab.agent_status, tab.important))
            }),
        })
    }

    /// The tick: pull `news.get` when due, and repaint a `running Xm` row
    /// on the next minute.
    pub(crate) fn tick_news(&mut self, now: std::time::Instant, outcome: &mut ClientShellInput) {
        let Some(signature) = self.news_snapshot_signature() else {
            return;
        };
        if self
            .news
            .pulled_for
            .as_ref()
            .is_some_and(|pulled| pulled.boot_id != signature.boot_id)
        {
            // Another connection: nothing of the old server's news applies.
            self.news = ClientNewsState::default();
            outcome.repaint = true;
        }
        let stale = self.news.pulled_for.as_ref() != Some(&signature);
        let periodic = self
            .news
            .last_pull
            .is_some_and(|last| now.duration_since(last) >= NEWS_REFRESH_INTERVAL);
        if (stale || self.news.refresh_due || periodic) && !self.news.loading {
            self.queue_news_get(signature, now, outcome);
        }
        let running_minutes = self.news.info.as_ref().and_then(|info| {
            info.run
                .as_ref()
                .map(|run| unix_now().saturating_sub(run.started_at) / 60)
        });
        if running_minutes != self.news.drawn_running_minutes {
            self.news.drawn_running_minutes = running_minutes;
            outcome.repaint |= self.news_row().is_some();
        }
    }

    /// When the tick loop must wake for the news row: the next minute of a
    /// run in flight.
    pub(crate) fn next_news_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let run = self.news.info.as_ref()?.run.as_ref()?;
        let elapsed = unix_now().saturating_sub(run.started_at);
        let until_next_minute = 60 - elapsed % 60;
        Some(now + std::time::Duration::from_secs(until_next_minute))
    }

    /// `news.get`, remembered for `signature`; silently skipped on an
    /// endpoint that does not advertise it or is offline.
    fn queue_news_get(
        &mut self,
        signature: NewsSnapshotSignature,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) {
        self.news.refresh_due = false;
        self.news.pulled_for = Some(signature);
        self.news.last_pull = Some(now);
        let method = Method::NewsGet(EmptyParams::default());
        if !self.supports_endpoint_method(&method)
            || !self.endpoint_is_online(&self.active_endpoint_id)
        {
            return;
        }
        if self.push_endpoint_method_with_kind(method, PendingEndpointKind::NewsGet, outcome) {
            self.news.loading = true;
        }
    }

    /// Ask for a fresh `news.get` on the next tick.
    pub(super) fn refresh_news(&mut self) {
        self.news.refresh_due = true;
    }

    /// Pull `news.get` now (the settings section); while one is in flight,
    /// on the next tick.
    pub(super) fn pull_news_now(&mut self, outcome: &mut ClientShellInput) {
        if self.news.loading {
            self.news.refresh_due = true;
            return;
        }
        if let Some(signature) = self.news_snapshot_signature() {
            self.queue_news_get(signature, std::time::Instant::now(), outcome);
        }
    }

    /// Right-click on the row: Run now, Open, Pause/Resume schedule.
    pub(super) fn open_news_context_menu(&mut self, x: u16, y: u16) {
        if self.news_row().is_none() {
            return;
        }
        let enabled = self.news.info.as_ref().is_some_and(|info| info.enabled);
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::News { enabled },
            x,
            y,
            highlighted: 0,
        }));
    }

    /// The News row menu's items.
    pub(super) fn activate_news_context_action(
        &mut self,
        enabled: bool,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            ClientContextMenuAction::NewsRun => {
                self.push_endpoint_method_with_kind(
                    Method::NewsRun(EmptyParams::default()),
                    PendingEndpointKind::NewsRun,
                    outcome,
                );
            }
            ClientContextMenuAction::NewsOpen => {
                self.push_endpoint_method_with_kind(
                    Method::NewsOpen(NewsOpenParams::default()),
                    PendingEndpointKind::NewsOpen,
                    outcome,
                );
            }
            ClientContextMenuAction::NewsToggleSchedule => {
                self.push_endpoint_method_with_kind(
                    Method::NewsSetEnabled(NewsSetEnabledParams { enabled: !enabled }),
                    PendingEndpointKind::NewsSetEnabled,
                    outcome,
                );
            }
            _ => {}
        }
    }

    /// `news.*` replies: `news.get` fills the state; the others answer with
    /// the same record (or a status) and a fresh pull follows either way.
    /// Errors already raised the generic notice.
    pub(super) fn handle_news_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        match kind {
            PendingEndpointKind::NewsGet => {
                self.news.loading = false;
                self.news.info = match result {
                    Ok(crate::api::schema::ResponseResult::NewsGet { news }) => Some(news),
                    Ok(_) => {
                        self.set_endpoint_error("endpoint returned an unexpected news result");
                        None
                    }
                    Err(_) => None,
                };
                // The reply names the News tab, which the signature then
                // tracks: seat it on the current snapshot so learning the
                // tab is not itself a change.
                self.news.pulled_for = self.news_snapshot_signature();
                self.sync_news_settings();
                (true, Vec::new())
            }
            _ => {
                if let Ok(crate::api::schema::ResponseResult::NewsGet { news }) = result {
                    self.news.info = Some(news);
                }
                self.refresh_news();
                (true, Vec::new())
            }
        }
    }
}
