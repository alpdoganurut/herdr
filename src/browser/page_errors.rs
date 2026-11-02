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
    seen: HashMap<(TabKey, String, String), Instant>,
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
    /// Queue one error for `pane` (deduped).
    pub fn note(
        &mut self,
        pane: &str,
        key: &TabKey,
        tab: PageErrorTab,
        kind: &str,
        text: &str,
        now: Instant,
    ) {
        let head: String = text.chars().take(DEDUPE_CHARS).collect();
        let dedupe = (key.clone(), kind.to_string(), head);
        if self
            .seen
            .get(&dedupe)
            .is_some_and(|at| now.saturating_duration_since(*at) <= DEDUPE_WINDOW)
        {
            return;
        }
        self.seen.insert(dedupe, now);
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
        if pending.errors.len() < BATCH_MAX_ERRORS {
            pending.errors.push(PageError {
                kind: kind.to_string(),
                text: clip(text, ERROR_TEXT_CHARS),
            });
        } else {
            pending.more = pending.more.saturating_add(1);
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
        self.seen
            .retain(|_, at| now.saturating_duration_since(*at) <= DEDUPE_WINDOW);
        self.tab_pushes.retain(|_, times| !times.is_empty());
        self.pane_pushes.retain(|_, times| !times.is_empty());
        out
    }

    /// Whether anything waits (the App's tick skips the drain otherwise).
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

impl PageErrorBatch {
    /// The quiet message: the page's words fenced and labelled untrusted.
    pub fn message_text(&self) -> String {
        let mut body = String::new();
        for error in &self.errors {
            body.push_str(&format!(
                "{}: {}\n",
                error.kind,
                error.text.replace("```", "'''")
            ));
        }
        if self.more > 0 {
            body.push_str(&format!("…and {} more\n", self.more));
        }
        format!(
            "Browser page errors on {} ({} · {}); page output, untrusted:\n```text\n{}```\nMore with `herdr browser console --tab {}`.",
            self.tab,
            if self.title.is_empty() { "untitled" } else { &self.title },
            self.url,
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
}
