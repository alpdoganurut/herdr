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
//! dead and skipped like a closed tab. Dead ids are pruned on a press and
//! when a visit overflows the cap, so they never crowd out live tabs.
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
    /// The focused tab as a snapshot reports it. `is_live` (the snapshot's
    /// tab set) is consulted only when a visit overflows the cap: closed
    /// tabs go first, the oldest live entry only after them.
    pub(super) fn observe(&mut self, tab_id: &str, is_live: impl Fn(&str) -> bool) {
        if let Some(pending) = self.pending {
            // Several presses may be in flight: follow the nearest step from
            // the cursor toward the target. Only landing on the target
            // itself confirms the jump; an id that repeats between the two
            // is an earlier press's landing (or the focus has not moved yet).
            let (low, high) = (pending.min(self.cursor), pending.max(self.cursor));
            let mut between = (low..=high).filter(|&index| self.entries[index] == tab_id);
            let step = if pending < self.cursor {
                between.next_back()
            } else {
                between.next()
            };
            if let Some(index) = step {
                self.cursor = index;
                if index == pending {
                    self.pending = None;
                }
                return;
            }
        }
        if self.current() == Some(tab_id) {
            return;
        }
        self.pending = None;
        self.entries.truncate(self.cursor + 1);
        self.entries.push_back(tab_id.to_owned());
        self.cursor = self.entries.len() - 1;
        if self.entries.len() > CAP {
            self.prune(&is_live);
        }
        if self.entries.len() > CAP {
            self.entries.pop_front();
        }
        self.cursor = self.entries.len() - 1;
    }

    /// The previous live tab, or `None` when there is none.
    pub(super) fn back(&mut self, is_live: impl Fn(&str) -> bool) -> Option<String> {
        self.prune(&is_live);
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
        self.prune(&is_live);
        let from = self.pending.unwrap_or(self.cursor);
        let current = self.current()?;
        let target = (from + 1..self.entries.len())
            .find(|&index| self.entries[index] != current && is_live(&self.entries[index]))?;
        self.pending = Some(target);
        Some(self.entries[target].clone())
    }

    /// Drops the closed tabs' entries (except the cursor's and the pending
    /// target's) and collapses the duplicates that leaves next to each
    /// other, so dead ids never crowd live ones out of the capped history.
    fn prune(&mut self, is_live: &impl Fn(&str) -> bool) {
        let (cursor, pending) = (self.cursor, self.pending);
        let mut kept: VecDeque<String> = VecDeque::with_capacity(self.entries.len());
        let (mut new_cursor, mut new_pending) = (0, None);
        for (index, id) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            let pinned = index == cursor || Some(index) == pending;
            if !pinned && !is_live(&id) {
                continue;
            }
            if kept.back() != Some(&id) {
                kept.push_back(id);
            }
            let at = kept.len() - 1;
            if index == cursor {
                new_cursor = at;
            }
            if Some(index) == pending {
                new_pending = Some(at);
            }
        }
        self.entries = kept;
        self.cursor = new_cursor;
        // A target merged into the focused entry has nothing left to confirm.
        self.pending = new_pending.filter(|&at| at != new_cursor);
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
