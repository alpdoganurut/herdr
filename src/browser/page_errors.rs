//! Page errors to the agent (fork, companion browser): the hub queues the
//! sidecar's `page_error` / `console_error` / `load_failed` / `crashed`
//! events of a tab per recipient pane; the App drains due batches on its
//! housekeeping tick and stores each as one quiet message (Claude agents;
//! others pull with `herdr browser console`).
//!
//! Batching and caps: one batch per (pane, tab) per [`BATCH_WINDOW`] with
//! at most [`BATCH_MAX_ERRORS`] errors; an error repeated within
//! [`DEDUPE_WINDOW`] (same tab, kind and first 200 chars) is dropped; at
//! most [`TAB_PUSHES`] batches per tab per [`TAB_PUSH_WINDOW`] and
//! [`PANE_PUSHES`] per pane per [`PANE_PUSH_WINDOW`]. Pure data.
//!
//! Scope: only a page the agent builds or tests sends it notices (an HTML
//! view from `browser show`, or a URL [`notice_url`] accepts: `file:`, a
//! loopback host, `0.0.0.0`); another site's errors stay in the tab's console for
//! `herdr browser console`. The App stores at most one batch per agent and
//! tab: a new one supersedes the one still waiting
//! (src/app/quiet_messages.rs, [`PageErrorBatch::coalesce_key`]).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::state::TabKey;

pub const BATCH_WINDOW: Duration = Duration::from_secs(3);
pub const BATCH_MAX_ERRORS: usize = 10;
pub const DEDUPE_WINDOW: Duration = Duration::from_secs(600);
pub const DEDUPE_CHARS: usize = 200;
pub const TAB_PUSHES: usize = 5;
pub const TAB_PUSH_WINDOW: Duration = Duration::from_secs(600);
pub const PANE_PUSHES: usize = 20;
pub const PANE_PUSH_WINDOW: Duration = Duration::from_secs(3600);
/// One error's text in a batch.
pub const ERROR_TEXT_CHARS: usize = 300;
/// At most this many dedupe entries per tab (bounded memory under a
/// chatty page; past it, repeats are no longer recognised).
pub const SEEN_PER_TAB: usize = 64;

/// One error event, as the hub saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageError {
    pub kind: String,
    pub text: String,
}

/// A due batch for one pane and one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageErrorBatch {
    pub pane: String,
    /// `profile:tN`.
    pub tab: String,
    pub url: String,
    pub title: String,
    pub errors: Vec<PageError>,
    /// Errors past the batch's cap.
    pub more: u32,
}

/// The tab facts a queued error carries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageErrorTab {
    pub tab: String,
    pub url: String,
    pub title: String,
}

#[derive(Debug)]
struct Pending {
    first_at: Instant,
    tab: PageErrorTab,
    errors: Vec<PageError>,
    more: u32,
}

#[derive(Debug, Default)]
pub struct PageErrorQueue {
    pending: BTreeMap<(String, TabKey), Pending>,
    /// Per tab: (kind, first chars) -> when it was last queued.
    seen: HashMap<TabKey, HashMap<(String, String), Instant>>,
    tab_pushes: HashMap<TabKey, VecDeque<Instant>>,
    pane_pushes: HashMap<String, VecDeque<Instant>>,
}

fn clip(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c == '\n' || !c.is_control() { c } else { ' ' })
        .collect();
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

fn prune(times: &mut VecDeque<Instant>, window: Duration, now: Instant) {
    while times
        .front()
        .is_some_and(|at| now.saturating_duration_since(*at) > window)
    {
        times.pop_front();
    }
}

impl PageErrorQueue {
    /// Queue one error for `pane` (deduped). An error that cannot reach
    /// the agent (the batch is full, the tab's or pane's push cap is
    /// spent) is only counted: it never enters the dedupe map.
    pub fn note(
        &mut self,
        pane: &str,
        key: &TabKey,
        tab: PageErrorTab,
        kind: &str,
        text: &str,
        now: Instant,
    ) {
        let spent = |times: Option<&VecDeque<Instant>>, cap: usize, window: Duration| {
            times.is_some_and(|times| {
                times
                    .iter()
                    .filter(|at| now.saturating_duration_since(**at) <= window)
                    .count()
                    >= cap
            })
        };
        if spent(self.tab_pushes.get(key), TAB_PUSHES, TAB_PUSH_WINDOW)
            || spent(self.pane_pushes.get(pane), PANE_PUSHES, PANE_PUSH_WINDOW)
        {
            return;
        }
        let head: String = text.chars().take(DEDUPE_CHARS).collect();
        let dedupe = (kind.to_string(), head);
        if self
            .seen
            .get(key)
            .and_then(|seen| seen.get(&dedupe))
            .is_some_and(|at| now.saturating_duration_since(*at) <= DEDUPE_WINDOW)
        {
            return;
        }
        let pending = self
            .pending
            .entry((pane.to_string(), key.clone()))
            .or_insert_with(|| Pending {
                first_at: now,
                tab: PageErrorTab::default(),
                errors: Vec::new(),
                more: 0,
            });
        pending.tab = tab;
        if pending.errors.len() >= BATCH_MAX_ERRORS {
            pending.more = pending.more.saturating_add(1);
            return;
        }
        pending.errors.push(PageError {
            kind: kind.to_string(),
            text: clip(text, ERROR_TEXT_CHARS),
        });
        let seen = self.seen.entry(key.clone()).or_default();
        if seen.len() >= SEEN_PER_TAB {
            seen.retain(|_, at| now.saturating_duration_since(*at) <= DEDUPE_WINDOW);
        }
        if seen.len() < SEEN_PER_TAB {
            seen.insert(dedupe, now);
        }
    }

    /// The batches whose window closed, within the tab and pane caps (the
    /// rest are dropped: the agent can still pull them).
    pub fn take_due(&mut self, now: Instant) -> Vec<PageErrorBatch> {
        let due: Vec<(String, TabKey)> = self
            .pending
            .iter()
            .filter(|(_, pending)| now.saturating_duration_since(pending.first_at) >= BATCH_WINDOW)
            .map(|(key, _)| key.clone())
            .collect();
        let mut out = Vec::new();
        for key in due {
            let Some(pending) = self.pending.remove(&key) else {
                continue;
            };
            let (pane, tab_key) = key;
            let tab_times = self.tab_pushes.entry(tab_key).or_default();
            prune(tab_times, TAB_PUSH_WINDOW, now);
            let pane_times = self.pane_pushes.entry(pane.clone()).or_default();
            prune(pane_times, PANE_PUSH_WINDOW, now);
            if tab_times.len() >= TAB_PUSHES || pane_times.len() >= PANE_PUSHES {
                continue;
            }
            tab_times.push_back(now);
            pane_times.push_back(now);
            out.push(PageErrorBatch {
                pane,
                tab: pending.tab.tab,
                url: pending.tab.url,
                title: pending.tab.title,
                errors: pending.errors,
                more: pending.more,
            });
        }
        for seen in self.seen.values_mut() {
            seen.retain(|_, at| now.saturating_duration_since(*at) <= DEDUPE_WINDOW);
        }
        self.seen.retain(|_, seen| !seen.is_empty());
        self.tab_pushes.retain(|_, times| !times.is_empty());
        self.pane_pushes.retain(|_, times| !times.is_empty());
        out
    }

    /// Whether anything waits (the App's tick skips the drain otherwise).
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Whether a page at `url` is one the agent builds or tests, so its errors
/// are worth a notice: a `file:` URL, or `http(s)` on `localhost`,
/// `*.localhost`, `127.0.0.1`, `0.0.0.0` or `[::1]` (case-insensitive; port, user
/// info and a trailing dot ignored). Any other site, and every other
/// scheme, is not.
pub fn notice_url(url: &str) -> bool {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme == "file" {
        return true;
    }
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let Some(rest) = rest.strip_prefix("//") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if host_port.starts_with('[') {
        match host_port.find(']') {
            Some(end) => &host_port[..=end],
            None => return false,
        }
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]")
        || (host.len() > ".localhost".len() && host.ends_with(".localhost"))
}

/// Whether an error of `kind` with `text` on a tab at `url` is worth a
/// notice ([`notice_url`]). A `load_failed` is decided by the request's URL
/// that herdr's sidecar or the companion puts last in its text (not page
/// output): the event comes before the tab's `navigated`, so `url` is still
/// the previous page (and afterwards Chromium's error page). Only a text
/// without one (herdr's own "the page failed to load") falls back to `url`.
pub fn notice_worthy(url: &str, kind: &str, text: &str) -> bool {
    if kind == "load_failed" {
        if let Some(request) = text
            .split_whitespace()
            .next_back()
            .filter(|word| is_url(word))
        {
            return notice_url(request);
        }
    }
    notice_url(url)
}

/// Whether `word` has a URL's shape: a scheme of letters, digits, `+`, `-`
/// or `.` starting with a letter, then `://` (or `file:`).
fn is_url(word: &str) -> bool {
    let Some((scheme, rest)) = word.split_once(':') else {
        return false;
    };
    scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && (rest.starts_with("//") || scheme.eq_ignore_ascii_case("file"))
}

/// A page label on the header line: no control characters, one line.
fn header_text(text: &str) -> String {
    crate::agents_model::envelope::strip_controls(text).replace(['\n', '\t'], " ")
}

impl PageErrorBatch {
    /// The quiet store's coalescing key: a new batch for the same agent and
    /// tab supersedes the one still waiting.
    pub fn coalesce_key(&self) -> String {
        format!("browser-page-errors:{}", self.tab)
    }

    /// The quiet message: the page's words fenced and labelled untrusted
    /// (tests; the App uses [`Self::message_text_replacing`]).
    #[cfg(test)]
    pub fn message_text(&self) -> String {
        self.message_text_replacing(0)
    }

    /// [`Self::message_text`] for a batch that supersedes `replaced` unread
    /// notices for its tab: the last line says so and where all errors are.
    pub fn message_text_replacing(&self, replaced: usize) -> String {
        let mut body = String::new();
        for error in &self.errors {
            body.push_str(&format!(
                "{}: {}\n",
                error.kind,
                crate::agents_model::envelope::message_text(&error.text).replace("```", "'''")
            ));
        }
        if self.more > 0 {
            body.push_str(&format!("…and {} more\n", self.more));
        }
        let more = match replaced {
            0 => "More with".to_string(),
            1 => "Replaces an earlier unread notice for this tab; all errors with".to_string(),
            n => format!("Replaces {n} earlier unread notices for this tab; all errors with"),
        };
        format!(
            "Browser page errors on {} ({} · {}); page output, untrusted:\n```text\n{}```\n{more} `herdr browser console --tab {}`.",
            self.tab,
            if self.title.is_empty() { "untitled".to_string() } else { header_text(&self.title) },
            header_text(&self.url),
            body,
            self.tab
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> PageErrorTab {
        PageErrorTab {
            tab: "companion:t1".into(),
            url: "https://x.test/".into(),
            title: "X".into(),
        }
    }

    #[test]
    fn fork_smoke_page_error_notices_are_for_files_and_loopback_hosts_only() {
        for url in [
            "file:///Users/me/report.html",
            "FILE://localhost/tmp/x.html",
            "http://localhost/",
            "http://localhost:5173/app?x=1#y",
            "HTTPS://LocalHost.:8443",
            "http://127.0.0.1:8080/",
            "http://user:pw@127.0.0.1/",
            "http://[::1]:3000/",
            "http://[::1]",
            "http://0.0.0.0:8000/",
            "http://app.localhost:3000/",
            "  http://localhost  ",
        ] {
            assert!(notice_url(url), "{url}");
        }
        for url in [
            "https://apps.apple.com/us/app/x",
            "https://example.com/",
            "https://localhost.evil.com/",
            "http://127.0.0.1.nip.io/",
            "http://localhost@evil.com/",
            "http://evil.com/?next=http://localhost/",
            "http://evil.com/#localhost",
            "http://.localhost/",
            "http://[::2]/",
            "http://127.0.0.2/",
            "http://0.0.0.0.evil.com/",
            "about:blank",
            "chrome-error://chromewebdata/",
            "data:text/html,hi",
            "localhost:3000",
            "",
        ] {
            assert!(!notice_url(url), "{url}");
        }
        // A failed load names the request's URL last; only `load_failed`
        // looks at the text (page output never decides).
        let page = "chrome-error://chromewebdata/";
        assert!(notice_worthy(
            page,
            "load_failed",
            "net::ERR_CONNECTION_REFUSED http://localhost:3000/"
        ));
        assert!(!notice_worthy(
            page,
            "load_failed",
            "net::ERR_FAILED https://x.test/"
        ));
    }

    #[test]
    fn fork_smoke_a_failed_load_is_decided_by_its_request_url() {
        // The tab is still on the local page when an external load fails
        // (the event comes before `navigated`).
        for text in [
            "net::ERR_NAME_NOT_RESOLVED https://cdn.external.test/",
            "HTTP 502 https://api.external.test/v1",
        ] {
            assert!(
                !notice_worthy("http://localhost:5173/", "load_failed", text),
                "{text}"
            );
        }
        // No URL in the text (herdr's own wording): the tab's URL decides.
        assert!(notice_worthy(
            "http://localhost:5173/",
            "load_failed",
            "the page failed to load"
        ));
        assert!(!notice_worthy(
            "https://x.test/",
            "load_failed",
            "the page failed to load"
        ));
    }

    #[test]
    fn page_output_never_decides_a_notice() {
        assert!(!notice_worthy(
            "https://x.test/",
            "console_error",
            "fetch http://localhost:3000/"
        ));
    }

    #[test]
    fn errors_wait_for_the_window_then_come_in_one_batch() {
        let mut queue = PageErrorQueue::default();
        let key = TabKey::new("companion", "T1");
        let t0 = Instant::now();
        queue.note("w1:p1", &key, tab(), "page_error", "boom", t0);
        queue.note("w1:p1", &key, tab(), "console_error", "bang", t0);
        assert!(queue.take_due(t0 + Duration::from_secs(1)).is_empty());
        let due = queue.take_due(t0 + BATCH_WINDOW);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].errors.len(), 2);
        let text = due[0].message_text();
        assert!(text.contains("untrusted") && text.contains("```text\npage_error: boom"));
        assert!(queue.is_empty());
    }

    fn seen_len(queue: &PageErrorQueue) -> usize {
        queue.seen.values().map(HashMap::len).sum()
    }

    #[test]
    fn errors_past_the_batch_cap_are_counted_but_never_remembered() {
        let mut queue = PageErrorQueue::default();
        let key = TabKey::new("companion", "T1");
        let t0 = Instant::now();
        for n in 0..1000 {
            queue.note("w1:p1", &key, tab(), "console_error", &format!("e{n}"), t0);
        }
        assert_eq!(seen_len(&queue), BATCH_MAX_ERRORS);
        let due = queue.take_due(t0 + BATCH_WINDOW);
        assert_eq!(due[0].errors.len(), BATCH_MAX_ERRORS);
        assert_eq!(due[0].more as usize, 1000 - BATCH_MAX_ERRORS);
    }

    #[test]
    fn a_tab_whose_pushes_are_spent_queues_and_remembers_nothing() {
        let mut queue = PageErrorQueue::default();
        let key = TabKey::new("companion", "T1");
        let mut now = Instant::now();
        for n in 0..TAB_PUSHES {
            queue.note("w1:p1", &key, tab(), "page_error", &format!("p{n}"), now);
            now += BATCH_WINDOW;
            assert_eq!(queue.take_due(now).len(), 1);
        }
        let before = seen_len(&queue);
        for n in 0..500 {
            queue.note("w1:p1", &key, tab(), "page_error", &format!("late{n}"), now);
        }
        assert!(queue.is_empty());
        assert_eq!(seen_len(&queue), before);
    }

    #[test]
    fn the_dedupe_map_is_capped_per_tab() {
        let mut queue = PageErrorQueue::default();
        let key = TabKey::new("companion", "T1");
        let now = Instant::now();
        // many panes erroring on one tab, each with a full batch pending
        for pane in 0..20 {
            for n in 0..BATCH_MAX_ERRORS {
                queue.note(
                    &format!("w1:p{pane}"),
                    &key,
                    tab(),
                    "page_error",
                    &format!("{pane}-{n}"),
                    now,
                );
            }
        }
        assert_eq!(queue.seen[&key].len(), SEEN_PER_TAB);
    }
}
