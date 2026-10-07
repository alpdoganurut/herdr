//! Fork (sidebar v3): browser-style tab history for `keys.tab_history_back` /
//! `keys.tab_history_forward` (default `cmd+[` / `cmd+]`).
//!
//! TUI navigation state, like `previous_pane_id`: client-only, per attached
//! client, for the active endpoint only, never sent to the server. Pure data,
//! no I/O.
//!
//! The history is one list of visited tab ids with a cursor on the focused
//! one. Every accepted snapshot reports its focused tab through [`observe`]:
//! a focus change from any source (a click, a keybind, an agent's
//! `tab.focus`, a notification jump, a fixed row such as News, the
//! coordinator or the Browser tab, a tab in another space) is a visit. A
//! visit drops everything after the cursor (the forward entries), appends
//! the tab and moves the cursor onto it. A snapshot whose focused tab is the
//! one under the cursor changes nothing.
//!
//! [`back`] and [`forward`] only pick a target: they walk from the in-flight
//! target (or the cursor) to the next live tab that is not the focused one,
//! remember it as pending and return it for a `tab.focus`. Nothing moves
//! until a snapshot confirms the jump, so repeated presses walk further, a
//! jump that never lands costs nothing, and the landing snapshot is not a
//! visit. A tab moved to another group gets a new public id; its old id is
//! dead and skipped like a closed tab.
//!
//! [`observe`]: TabHistory::observe
//! [`back`]: TabHistory::back
//! [`forward`]: TabHistory::forward

use std::collections::VecDeque;

/// Entries kept; the oldest is dropped past it.
pub(super) const CAP: usize = 50;

#[derive(Debug, Default)]
pub(super) struct TabHistory {
    /// Visited tab ids, oldest first, consecutive duplicates collapsed.
    entries: VecDeque<String>,
    /// The index of the focused tab (meaningless while `entries` is empty).
    cursor: usize,
    /// The index a back/forward jump targets until a snapshot confirms it.
    pending: Option<usize>,
}

impl TabHistory {
    /// The focused tab as a snapshot reports it.
    pub(super) fn observe(&mut self, tab_id: &str) {
        if let Some(pending) = self.pending {
            if self.entries[pending] == tab_id {
                // The jump landed: move the cursor without recording a visit.
                self.cursor = pending;
                self.pending = None;
                return;
            }
            // Several presses in flight: an earlier one's target landed first
            // (or the focus has not moved yet). Follow it, keep waiting.
            let (low, high) = (pending.min(self.cursor), pending.max(self.cursor));
            let mut between = (low..=high).filter(|&index| self.entries[index] == tab_id);
            let step = if pending < self.cursor {
                between.next_back()
            } else {
                between.next()
            };
            if let Some(index) = step {
                self.cursor = index;
                return;
            }
        }
        if self.current() == Some(tab_id) {
            return;
        }
        self.pending = None;
        self.entries.truncate(self.cursor + 1);
        self.entries.push_back(tab_id.to_owned());
        if self.entries.len() > CAP {
            self.entries.pop_front();
        }
        self.cursor = self.entries.len() - 1;
    }

    /// The previous live tab, or `None` when there is none.
    pub(super) fn back(&mut self, is_live: impl Fn(&str) -> bool) -> Option<String> {
        let from = self.pending.unwrap_or(self.cursor);
        let current = self.current()?;
        let target = (0..from)
            .rev()
            .find(|&index| self.entries[index] != current && is_live(&self.entries[index]))?;
        self.pending = Some(target);
        Some(self.entries[target].clone())
    }

    /// The next live tab after going back, or `None` when there is none.
    pub(super) fn forward(&mut self, is_live: impl Fn(&str) -> bool) -> Option<String> {
        let from = self.pending.unwrap_or(self.cursor);
        let current = self.current()?;
        let target = (from + 1..self.entries.len())
            .find(|&index| self.entries[index] != current && is_live(&self.entries[index]))?;
        self.pending = Some(target);
        Some(self.entries[target].clone())
    }

    /// Forget everything (a new server boot or another endpoint).
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    fn current(&self) -> Option<&str> {
        self.entries.get(self.cursor).map(String::as_str)
    }

    #[cfg(test)]
    pub(super) fn entries(&self) -> Vec<&str> {
        self.entries.iter().map(String::as_str).collect()
    }

    #[cfg(test)]
    pub(super) fn focused(&self) -> Option<&str> {
        self.current()
    }
}
