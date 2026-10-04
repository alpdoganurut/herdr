//! Derived state of the `tabs` sidebar (fork, sidebar v2): per-tab facts,
//! the list rows and the Active agents list, computed on data change and
//! read by the row painters in O(1).
//!
//! `SidebarModel::ensure` runs in `compose` for the expanded tabs layout. It
//! rebuilds only after `mark_dirty` (snapshot replaced, voice / agent-times /
//! teams push, config reload, fold state changed) or when the pinned tab ids
//! (News, coordinator) differ from the last build; every other frame costs
//! one bool check and two `Option<&str>` compares. The rebuild is one pass
//! over the snapshot's agents and panes and one over its tabs.
//!
//! Also here, shared by the row painters (`tab_sidebar.rs`) and the Active
//! block / detail strip (`tab_sidebar_active.rs`, `tab_sidebar_detail.rs`):
//! the selected-row rule (`resolve_selected`), the tab status icon
//! (`tab_row_icon`), the hover target (`SidebarHover`) and `StackStr`.

// Sidebar v2 S0b: the Active block, detail strip and hover wiring read the
// rest of this model in later steps; until then parts are unused.
#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Instant;

use super::*;
use crate::api::schema::{AgentStatus, AgentVoiceMode};

/// Facts about one tab, aligned with `snapshot.tabs` by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TabFacts {
    /// Index into `snapshot.agents` of the tab's last agent (its glyph).
    pub(crate) primary_agent: Option<u32>,
    /// Running subagents over agents whose status shows them.
    pub(crate) subagents: u32,
    /// The loudest voice mode over the tab's agents (`voice::louder`).
    pub(crate) voice: Option<AgentVoiceMode>,
    /// The lowest `state_change_seq` of the agents whose status is the
    /// tab's (`u64::MAX` when none, a seq of 0 counts as `u64::MAX`).
    pub(crate) status_seq: u64,
    /// The earliest known time those agents entered the state.
    pub(crate) since: Option<Instant>,
    pub(crate) agents: u16,
    /// `snapshot.panes` of this tab.
    pub(crate) panes: u16,
    /// The News or coordinator tab (never a list row).
    pub(crate) pinned: bool,
}

impl Default for TabFacts {
    fn default() -> Self {
        Self {
            primary_agent: None,
            subagents: 0,
            voice: None,
            status_seq: u64::MAX,
            since: None,
            agents: 0,
            panes: 0,
            pinned: false,
        }
    }
}

/// One Active agents entry: an index into `snapshot.tabs` and its class
/// (0 blocked, 1 voice, 2 working, 3 done).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveEntry {
    pub(crate) tab: u32,
    pub(crate) class: u8,
}

/// One list row, by index into the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Row {
    /// A group header: `snapshot.workspaces[workspace]`, its fold state and
    /// its non-pinned member count.
    Header {
        workspace: u32,
        folded: bool,
        members: u16,
    },
    /// A tab row: `snapshot.tabs[tab]`.
    Tab { tab: u32 },
}

/// The Active agents block's view state for one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActiveView {
    /// `ui.sidebar_active_agents`.
    pub(crate) enabled: bool,
    /// Folded to its header (persisted).
    pub(crate) folded: bool,
    /// Showing more than the cap (`+N more` clicked; client-only).
    pub(crate) expanded: bool,
}

/// A pinned row under the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PinnedKind {
    Browser,
    News,
    Coordinator,
}

/// What the pointer hovers in the tabs sidebar (client-only, set on mouse
/// motion). Ids are validated against the snapshot when read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarHover {
    Tab(String),
    Group(String),
    ActiveHeader,
    ActiveEntry(String),
    ActiveMore,
    Pinned(PinnedKind),
}

/// The selected row of a frame: the hovered row, else the focused tab.
/// Indices are into `snapshot.tabs` / `snapshot.workspaces`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selected {
    Tab(u32),
    Group(u32),
    ActiveHeader,
    ActiveEntry(u32),
    Pinned(PinnedKind),
    None,
}

/// The `tabs` sidebar's derived state (see the module docs).
#[derive(Debug)]
pub(crate) struct SidebarModel {
    /// Data changed since the last build (starts dirty: never built).
    dirty: bool,
    /// The News and coordinator tab ids of the last build.
    pinned_ids: (Option<String>, Option<String>),
    /// Per-tab facts, aligned with `snapshot.tabs`.
    pub(crate) tabs: Vec<TabFacts>,
    /// The list rows, fold state applied; the focused tab's group is open.
    pub(crate) rows: Vec<Row>,
    /// All 1, `rows.len()` long (`scroll::list_scroll_metrics`).
    pub(crate) row_heights: Vec<u16>,
    /// 1 after the row above every header that has one (spacer rows).
    pub(crate) gaps_after: Vec<u16>,
    /// All 0, `rows.len()` long (no spacer rows).
    pub(crate) no_gaps: Vec<u16>,
    /// The focused tab's index, when it is a list tab.
    pub(crate) focused_tab: Option<u32>,
    /// Active agents entries, sorted; empty when the block is disabled.
    pub(crate) active: Vec<ActiveEntry>,
    /// Bitset of the classes present in `active` (bit `class`).
    pub(crate) active_classes: u8,
    /// Some active entry has running subagents.
    pub(crate) any_subagents_active: bool,
    /// Rebuild count (architecture tests).
    #[cfg(test)]
    pub(crate) builds: u32,
}

impl Default for SidebarModel {
    /// Never built: the first `ensure` builds.
    fn default() -> Self {
        Self::new()
    }
}

impl SidebarModel {
    pub(crate) fn new() -> Self {
        Self {
            dirty: true,
            pinned_ids: (None, None),
            tabs: Vec::new(),
            rows: Vec::new(),
            row_heights: Vec::new(),
            gaps_after: Vec::new(),
            no_gaps: Vec::new(),
            focused_tab: None,
            active: Vec::new(),
            active_classes: 0,
            any_subagents_active: false,
            #[cfg(test)]
            builds: 0,
        }
    }

    /// A model built once, for a caller without the compose-ensured one.
    pub(crate) fn built(
        snapshot: &ClientShellSnapshot,
        collapsed_groups: &HashSet<String>,
        voice: Option<&super::voice::ClientVoiceState>,
        pinned_ids: (Option<&str>, Option<&str>),
        enabled: bool,
    ) -> Self {
        let mut model = Self::new();
        model.ensure(snapshot, collapsed_groups, voice, None, pinned_ids, enabled);
        model
    }

    /// The next `ensure` rebuilds.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Rebuild when data changed since the last build or the pinned tab ids
    /// differ; otherwise nothing.
    #[allow(clippy::too_many_arguments)] // the model's inputs; a struct would only rename them
    pub(crate) fn ensure(
        &mut self,
        snapshot: &ClientShellSnapshot,
        collapsed_groups: &HashSet<String>,
        voice: Option<&super::voice::ClientVoiceState>,
        times: Option<&super::agent_times::ClientAgentTimesState>,
        pinned_ids: (Option<&str>, Option<&str>),
        enabled: bool,
    ) {
        if !self.dirty
            && self.pinned_ids.0.as_deref() == pinned_ids.0
            && self.pinned_ids.1.as_deref() == pinned_ids.1
        {
            return;
        }
        self.rebuild(
            snapshot,
            collapsed_groups,
            voice,
            times,
            pinned_ids,
            enabled,
        );
    }

    fn rebuild(
        &mut self,
        snapshot: &ClientShellSnapshot,
        collapsed_groups: &HashSet<String>,
        voice: Option<&super::voice::ClientVoiceState>,
        times: Option<&super::agent_times::ClientAgentTimesState>,
        pinned_ids: (Option<&str>, Option<&str>),
        _enabled: bool,
    ) {
        self.dirty = false;
        if self.pinned_ids.0.as_deref() != pinned_ids.0 {
            self.pinned_ids.0 = pinned_ids.0.map(str::to_owned);
        }
        if self.pinned_ids.1.as_deref() != pinned_ids.1 {
            self.pinned_ids.1 = pinned_ids.1.map(str::to_owned);
        }
        #[cfg(test)]
        {
            self.builds = self.builds.saturating_add(1);
        }
        let is_pinned = |tab_id: &str| pinned_ids.0 == Some(tab_id) || pinned_ids.1 == Some(tab_id);

        // Per-tab facts: one pass over the agents and one over the panes.
        self.tabs.clear();
        self.tabs.extend(snapshot.tabs.iter().map(|tab| TabFacts {
            pinned: is_pinned(&tab.tab_id),
            ..TabFacts::default()
        }));
        let index: HashMap<&str, u32> = snapshot
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| (tab.tab_id.as_str(), index as u32))
            .collect();
        // No voice lookups while no pane is in voice mode.
        let voice = voice.filter(|voice| !voice.panes.is_empty());
        for (agent_index, agent) in snapshot.agents.iter().enumerate() {
            let Some(&tab_index) = index.get(agent.tab_id.as_str()) else {
                continue;
            };
            let tab_status = snapshot.tabs[tab_index as usize].agent_status;
            let facts = &mut self.tabs[tab_index as usize];
            facts.primary_agent = Some(agent_index as u32);
            facts.agents = facts.agents.saturating_add(1);
            if agent.subagents > 0 && shows_subagents(agent.agent_status) {
                facts.subagents = facts.subagents.saturating_add(agent.subagents);
            }
            if let Some(mode) = voice.and_then(|voice| voice.voice_of(&agent.pane_id)) {
                facts.voice = super::voice::louder(facts.voice, mode);
            }
            if agent.agent_status == tab_status {
                let seq = if agent.state_change_seq == 0 {
                    u64::MAX
                } else {
                    agent.state_change_seq
                };
                facts.status_seq = facts.status_seq.min(seq);
                if let Some(since) =
                    times.and_then(|times| times.since_of(&agent.pane_id, agent.state_change_seq))
                {
                    facts.since = Some(facts.since.map_or(since, |current| current.min(since)));
                }
            }
        }
        for pane in &snapshot.panes {
            if let Some(&tab_index) = index.get(pane.tab_id.as_str()) {
                let facts = &mut self.tabs[tab_index as usize];
                facts.panes = facts.panes.saturating_add(1);
            }
        }
        self.focused_tab = snapshot
            .tabs
            .iter()
            .position(|tab| tab.focused && !is_pinned(&tab.tab_id))
            .map(|index| index as u32);

        // The list rows: every group's header, then its tabs unless folded;
        // the first space is the ungrouped bucket (no header).
        let focused = snapshot
            .tabs
            .iter()
            .find(|tab| tab.focused)
            .map(|tab| tab.workspace_id.as_str());
        let position: HashMap<&str, usize> = snapshot
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| (workspace.workspace_id.as_str(), index))
            .collect();
        let mut members: Vec<Vec<u32>> = vec![Vec::new(); snapshot.workspaces.len()];
        for (tab_index, tab) in snapshot.tabs.iter().enumerate() {
            if is_pinned(&tab.tab_id) {
                continue;
            }
            if let Some(index) = position.get(tab.workspace_id.as_str()) {
                members[*index].push(tab_index as u32);
            }
        }
        self.rows.clear();
        for (index, (workspace, tabs)) in snapshot.workspaces.iter().zip(members).enumerate() {
            let mut folded = false;
            if super::tab_sidebar::is_group_index(index) {
                folded = focused != Some(workspace.workspace_id.as_str())
                    && collapsed_groups.contains(&group_key(&workspace.workspace_id));
                self.rows.push(Row::Header {
                    workspace: index as u32,
                    folded,
                    members: u16::try_from(tabs.len()).unwrap_or(u16::MAX),
                });
            }
            if !folded {
                self.rows
                    .extend(tabs.into_iter().map(|tab| Row::Tab { tab }));
            }
        }
        let rows = self.rows.len();
        self.row_heights.clear();
        self.row_heights.resize(rows, 1);
        self.no_gaps.clear();
        self.no_gaps.resize(rows, 0);
        self.gaps_after.clear();
        self.gaps_after.resize(rows, 0);
        for (index, row) in self.rows.iter().enumerate().skip(1) {
            if matches!(row, Row::Header { .. }) {
                self.gaps_after[index - 1] = 1;
            }
        }

        // The Active agents list is filled in by the Active block step.
        self.active.clear();
        self.active_classes = 0;
        self.any_subagents_active = false;
    }

    /// The facts of `snapshot.tabs[tab]`.
    pub(crate) fn tab(&self, tab: u32) -> Option<&TabFacts> {
        self.tabs.get(tab as usize)
    }
}

/// Whether a status gives way to the subagent icon while subagents run: a
/// working, idle or finished agent (background agents outlive the turn).
/// Blocked keeps its icon (needing you outranks), and suspended or unknown
/// agents have none.
pub(crate) fn shows_subagents(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Working | AgentStatus::Idle | AgentStatus::Done
    )
}

/// A tab's status icon: the subagent icon while subagents run under a
/// status that shows them, else the status icon in the configured style.
pub(crate) fn tab_row_icon(
    status: AgentStatus,
    subagents: u32,
    indicators: crate::config::StatusIndicatorStyle,
) -> &'static str {
    if shows_subagents(status) && subagents > 0 {
        super::tab_sidebar::TAB_SUBAGENTS_ICON
    } else {
        status_icon(status, indicators)
    }
}

/// The selected row: the hovered row while its target still exists in the
/// snapshot (a pinned tab id does not count), else the focused tab. A
/// stale hover counts as no hover.
pub(crate) fn resolve_selected(
    snapshot: &ClientShellSnapshot,
    model: &SidebarModel,
    hover: Option<&SidebarHover>,
) -> Selected {
    let list_tab = |tab_id: &str| {
        snapshot
            .tabs
            .iter()
            .position(|tab| tab.tab_id == tab_id)
            .filter(|index| model.tab(*index as u32).is_some_and(|facts| !facts.pinned))
            .map(|index| index as u32)
    };
    let hovered = match hover {
        Some(SidebarHover::Tab(tab_id)) => list_tab(tab_id).map(Selected::Tab),
        Some(SidebarHover::ActiveEntry(tab_id)) => list_tab(tab_id).map(Selected::ActiveEntry),
        Some(SidebarHover::Group(workspace_id)) => snapshot
            .workspaces
            .iter()
            .position(|workspace| workspace.workspace_id == *workspace_id)
            .map(|index| Selected::Group(index as u32)),
        Some(SidebarHover::ActiveHeader | SidebarHover::ActiveMore) => Some(Selected::ActiveHeader),
        Some(SidebarHover::Pinned(kind)) => Some(Selected::Pinned(*kind)),
        None => None,
    };
    hovered.unwrap_or_else(|| model.focused_tab.map_or(Selected::None, Selected::Tab))
}

/// A small stack string for durations and counts (`fmt::Write`; a write that
/// does not fit fails and leaves the content unchanged).
#[derive(Debug, Clone, Copy)]
pub(crate) struct StackStr<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackStr<N> {
    pub(crate) fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or_default()
    }
}

impl<const N: usize> Default for StackStr<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> std::fmt::Write for StackStr<N> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len + text.len();
        if end > N {
            return Err(std::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn stack_str_writes_until_full() {
        let mut text = StackStr::<5>::new();
        assert!(write!(text, "{}d{}h", 3, 4).is_ok());
        assert_eq!(text.as_str(), "3d4h");
        assert!(write!(text, "xy").is_err());
        assert_eq!(text.as_str(), "3d4h", "a write that does not fit leaves it");
    }

    #[test]
    fn tab_row_icon_shows_subagents_except_while_blocked() {
        use crate::config::StatusIndicatorStyle::Dots;
        let subagents = super::super::tab_sidebar::TAB_SUBAGENTS_ICON;
        assert_eq!(tab_row_icon(AgentStatus::Working, 2, Dots), subagents);
        assert_eq!(tab_row_icon(AgentStatus::Done, 1, Dots), subagents);
        assert_eq!(tab_row_icon(AgentStatus::Blocked, 2, Dots), "●");
        assert_eq!(tab_row_icon(AgentStatus::Working, 0, Dots), "●");
    }
}
