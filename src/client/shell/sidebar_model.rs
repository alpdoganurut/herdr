//! Derived state of the `tabs` sidebar (fork, sidebar v2): per-tab facts,
//! the list rows and the Active list, computed on data change and
//! read by the row painters in O(1).
//!
//! `SidebarModel::ensure` runs in `compose` for the expanded tabs layout. It
//! rebuilds only after `mark_dirty` (snapshot replaced, voice / agent-times /
//! teams / tab-pins push, config reload, fold state changed) or when the
//! fixed-row tab ids (News, coordinator) differ from the last build; every
//! other frame costs one bool check and two `Option<&str>` compares. The
//! rebuild is one pass over the snapshot's agents and panes and one over its
//! tabs (plus one set lookup per tab while some tab is pinned).
//!
//! Fork (sidebar v3): the rebuild also lists the Pinned entries (pinned
//! tabs, snapshot order) and the Scheduled entries (tabs with a scheduled
//! reminder, by interval then snapshot order). A countdown tick never
//! rebuilds: the reminder clocks are read live by the Scheduled block.
//!
//! Also here, shared by the row painters (`tab_sidebar.rs`) and the Active
//! block / detail strip (`tab_sidebar_active.rs`, `tab_sidebar_detail.rs`):
//! the selected-row rule (`resolve_selected`), the tab status icon
//! (`tab_row_icon`), the hover target (`SidebarHover`) and `StackStr`.

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
    /// The News or coordinator tab (a fixed row, never a list row).
    pub(crate) fixed: bool,
    /// Index into `snapshot.workspaces` of the tab's space.
    pub(crate) workspace: Option<u32>,
    /// Some agent of the tab is parked (`Suspended`).
    pub(crate) parked: bool,
    /// Fork (sidebar v3): pinned (`tab.set_pinned`, the `endpoint.tab-pins.v1`
    /// push).
    pub(crate) pin: bool,
    /// Fork: notifications muted (`tab.set_muted`, the
    /// `endpoint.tab-mutes.v1` push).
    pub(crate) muted: bool,
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
            fixed: false,
            workspace: None,
            parked: false,
            pin: false,
            muted: false,
        }
    }
}

/// One Active block entry: an index into `snapshot.tabs` and its class
/// (0 blocked, 1 voice, 2 working, 3 idle with subagents running, 4 done).
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
    /// Fork: a run of `len` suspended tabs in a row folded into one row (or
    /// its header while `expanded`, the tab rows following it). Its tabs are
    /// `SidebarModel::run_tabs[start..start + len]`.
    Run {
        start: u32,
        len: u16,
        expanded: bool,
    },
}

/// Fork: the fewest suspended tabs in a row that fold into a run.
pub(crate) const MIN_RUN: usize = 3;

/// The Active block's view state for one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActiveView {
    /// `ui.sidebar_active_agents`.
    pub(crate) enabled: bool,
    /// Folded to its header (persisted).
    pub(crate) folded: bool,
    /// Showing more than the cap (`+N more` clicked; client-only).
    pub(crate) expanded: bool,
}

/// The Pinned and Scheduled blocks' view state (sidebar v3), like Active's.
pub(crate) type PinsView = ActiveView;
pub(crate) type ScheduledView = ActiveView;

/// Which optional blocks the model lists (`ui.sidebar_*_agents`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sections {
    pub(crate) active: bool,
    pub(crate) pins: bool,
    pub(crate) scheduled: bool,
    /// Fork: `ui.sidebar_collapse_suspended`, runs of suspended tabs fold.
    pub(crate) suspended_runs: bool,
}

impl Default for Sections {
    fn default() -> Self {
        Self {
            active: true,
            pins: true,
            scheduled: true,
            suspended_runs: true,
        }
    }
}

/// The model's inputs.
#[derive(Clone, Copy)]
pub(crate) struct ModelInputs<'a> {
    pub(crate) snapshot: &'a ClientShellSnapshot,
    pub(crate) collapsed_groups: &'a HashSet<String>,
    pub(crate) voice: Option<&'a super::voice::ClientVoiceState>,
    pub(crate) times: Option<&'a super::agent_times::ClientAgentTimesState>,
    pub(crate) pins: Option<&'a super::tab_pins::ClientTabPinsState>,
    /// Fork: the muted tabs (the row's gutter mark).
    pub(crate) mutes: Option<&'a super::tab_mutes::ClientTabMutesState>,
    /// The News and coordinator tab ids (fixed rows, never list rows).
    pub(crate) fixed_ids: (Option<&'a str>, Option<&'a str>),
    pub(crate) sections: Sections,
    /// Fork: the expanded suspended runs, by their first tab's id.
    pub(crate) expanded_runs: Option<&'a HashSet<String>>,
}

impl<'a> ModelInputs<'a> {
    /// Inputs with nothing but the snapshot and fold state (tests and
    /// callers without pushes); every block enabled.
    pub(crate) fn new(
        snapshot: &'a ClientShellSnapshot,
        collapsed_groups: &'a HashSet<String>,
    ) -> Self {
        Self {
            snapshot,
            collapsed_groups,
            voice: None,
            times: None,
            pins: None,
            mutes: None,
            fixed_ids: (None, None),
            sections: Sections::default(),
            expanded_runs: None,
        }
    }
}

/// The Scheduled interval kinds (`scheduled_kinds` bits): minutes (`◷`),
/// hours (`◑`), daily (`☼`).
pub(crate) const KIND_MINUTES: u8 = 0;
pub(crate) const KIND_HOURS: u8 = 1;
pub(crate) const KIND_DAILY: u8 = 2;

/// An interval's kind bit and its display rank (5m, 10m, 30m, 1h, 6h,
/// daily); `None` for an unknown interval.
pub(crate) fn interval_rank(every: crate::api::schema::TabRemindInterval) -> Option<(u8, u8)> {
    use crate::api::schema::TabRemindInterval;
    Some(match every {
        TabRemindInterval::M5 => (KIND_MINUTES, 0),
        TabRemindInterval::M10 => (KIND_MINUTES, 1),
        TabRemindInterval::M30 => (KIND_MINUTES, 2),
        TabRemindInterval::H1 => (KIND_HOURS, 3),
        TabRemindInterval::H6 => (KIND_HOURS, 4),
        TabRemindInterval::Daily => (KIND_DAILY, 5),
        TabRemindInterval::Unknown => return None,
    })
}

/// A fixed row under the list (Browser, News, coordinator; "pinned" in the docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FixedKind {
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
    Fixed(FixedKind),
    PinsHeader,
    PinsEntry(String),
    PinsMore,
    ScheduledHeader,
    ScheduledEntry(String),
    ScheduledMore,
    /// Fork: a suspended run's row, by its first tab's id.
    Run(String),
}

/// The selected row of a frame: the hovered row, else the focused tab.
/// Indices are into `snapshot.tabs` / `snapshot.workspaces`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selected {
    Tab(u32),
    Group(u32),
    ActiveHeader,
    ActiveEntry(u32),
    Fixed(FixedKind),
    /// Fork (sidebar v3): the Pinned / Scheduled header (or its overflow
    /// row) and entries.
    PinsHeader,
    PinsEntry(u32),
    ScheduledHeader,
    ScheduledEntry(u32),
    /// Fork: a suspended run's row (`Row::Run`'s `start` and `len`).
    Run {
        start: u32,
        len: u16,
    },
    None,
}

/// The `tabs` sidebar's derived state (see the module docs).
#[derive(Debug)]
pub(crate) struct SidebarModel {
    /// Data changed since the last build (starts dirty: never built).
    dirty: bool,
    /// The News and coordinator tab ids of the last build.
    fixed_ids: (Option<String>, Option<String>),
    /// Per-tab facts, aligned with `snapshot.tabs`.
    pub(crate) tabs: Vec<TabFacts>,
    /// The list rows, fold state applied; the focused tab's group is open.
    pub(crate) rows: Vec<Row>,
    /// Fork: every suspended run's tabs, back to back (`Row::Run` slices it).
    pub(crate) run_tabs: Vec<u32>,
    /// All 1, `rows.len()` long (`scroll::list_scroll_metrics`).
    pub(crate) row_heights: Vec<u16>,
    /// 1 after the row above every header that has one (spacer rows).
    pub(crate) gaps_after: Vec<u16>,
    /// All 0, `rows.len()` long (no spacer rows).
    pub(crate) no_gaps: Vec<u16>,
    /// The focused tab's index, when it is a list tab.
    pub(crate) focused_tab: Option<u32>,
    /// Active entries, sorted; empty when the block is disabled.
    pub(crate) active: Vec<ActiveEntry>,
    /// Bitset of the classes present in `active` (bit `class`).
    pub(crate) active_classes: u8,
    /// Some active entry has running subagents.
    pub(crate) any_subagents_active: bool,
    /// Fork (sidebar v3): Pinned entries (indices into `snapshot.tabs`):
    /// pinned list tabs in snapshot order; empty when the block is disabled.
    pub(crate) pins: Vec<u32>,
    /// Bitset of the Active classes present over `pins` (folded header).
    pub(crate) pins_classes: u8,
    /// Fork (sidebar v3): Scheduled entries: list tabs with a known
    /// scheduled reminder, by interval (5m … daily) then snapshot order;
    /// empty when the block is disabled.
    pub(crate) scheduled: Vec<u32>,
    /// Bitset of the interval kinds present over `scheduled` (`KIND_*`).
    pub(crate) scheduled_kinds: u8,
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
            fixed_ids: (None, None),
            tabs: Vec::new(),
            rows: Vec::new(),
            run_tabs: Vec::new(),
            row_heights: Vec::new(),
            gaps_after: Vec::new(),
            no_gaps: Vec::new(),
            focused_tab: None,
            active: Vec::new(),
            active_classes: 0,
            any_subagents_active: false,
            pins: Vec::new(),
            pins_classes: 0,
            scheduled: Vec::new(),
            scheduled_kinds: 0,
            #[cfg(test)]
            builds: 0,
        }
    }

    /// A model built once, for a caller without the compose-ensured one.
    pub(crate) fn built(inputs: ModelInputs<'_>) -> Self {
        let mut model = Self::new();
        model.ensure(inputs);
        model
    }

    /// The next `ensure` rebuilds.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Rebuild when data changed since the last build or the fixed-row tab
    /// ids differ; otherwise nothing.
    pub(crate) fn ensure(&mut self, inputs: ModelInputs<'_>) {
        if !self.dirty
            && self.fixed_ids.0.as_deref() == inputs.fixed_ids.0
            && self.fixed_ids.1.as_deref() == inputs.fixed_ids.1
        {
            return;
        }
        self.rebuild(inputs);
    }

    fn rebuild(&mut self, inputs: ModelInputs<'_>) {
        let ModelInputs {
            snapshot,
            collapsed_groups,
            voice,
            times,
            pins,
            mutes,
            fixed_ids,
            sections,
            expanded_runs,
        } = inputs;
        self.dirty = false;
        if self.fixed_ids.0.as_deref() != fixed_ids.0 {
            self.fixed_ids.0 = fixed_ids.0.map(str::to_owned);
        }
        if self.fixed_ids.1.as_deref() != fixed_ids.1 {
            self.fixed_ids.1 = fixed_ids.1.map(str::to_owned);
        }
        #[cfg(test)]
        {
            self.builds = self.builds.saturating_add(1);
        }
        let is_fixed = |tab_id: &str| fixed_ids.0 == Some(tab_id) || fixed_ids.1 == Some(tab_id);

        // Per-tab facts: one pass over the agents and one over the panes.
        self.tabs.clear();
        // No pin lookups while nothing is pinned.
        let pins = pins.filter(|pins| !pins.tab_ids.is_empty());
        // Fork: likewise no mute lookups while nothing is muted.
        let mutes = mutes.filter(|mutes| !mutes.tab_ids.is_empty());
        self.tabs.extend(snapshot.tabs.iter().map(|tab| TabFacts {
            fixed: is_fixed(&tab.tab_id),
            pin: pins.is_some_and(|pins| pins.is_pinned(&tab.tab_id)),
            muted: mutes.is_some_and(|mutes| mutes.is_muted(&tab.tab_id)),
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
            facts.parked |= agent.agent_status == AgentStatus::Suspended;
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
            .position(|tab| tab.focused && !is_fixed(&tab.tab_id))
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
            let workspace = position.get(tab.workspace_id.as_str()).copied();
            self.tabs[tab_index].workspace = workspace.map(|index| index as u32);
            if is_fixed(&tab.tab_id) {
                continue;
            }
            if let Some(index) = workspace {
                members[index].push(tab_index as u32);
            }
        }
        self.rows.clear();
        self.run_tabs.clear();
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
                if sections.suspended_runs {
                    self.push_tab_rows(snapshot, &tabs, expanded_runs);
                } else {
                    self.rows
                        .extend(tabs.into_iter().map(|tab| Row::Tab { tab }));
                }
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

        // The Active list: blocked, then live voice, then working, then idle
        // with subagents running, then finished; within a class the longest
        // in its state first.
        self.active.clear();
        self.active_classes = 0;
        self.any_subagents_active = false;
        if sections.active {
            for (index, (tab, facts)) in snapshot.tabs.iter().zip(&self.tabs).enumerate() {
                if facts.fixed {
                    continue;
                }
                let Some(class) = active_class(tab.agent_status, facts.voice, facts.subagents)
                else {
                    continue;
                };
                self.active.push(ActiveEntry {
                    tab: index as u32,
                    class,
                });
                self.active_classes |= 1 << class;
                self.any_subagents_active |= facts.subagents > 0;
            }
            let tabs = &self.tabs;
            self.active.sort_unstable_by_key(|entry| {
                let seq = tabs
                    .get(entry.tab as usize)
                    .map_or(u64::MAX, |facts| facts.status_seq);
                (entry.class, seq, entry.tab)
            });
        }

        // Fork (sidebar v3): the Pinned entries, in snapshot order (pinned
        // tabs also stay in their group).
        self.pins.clear();
        self.pins_classes = 0;
        if sections.pins && pins.is_some() {
            for (index, (tab, facts)) in snapshot.tabs.iter().zip(&self.tabs).enumerate() {
                if facts.fixed || !facts.pin {
                    continue;
                }
                self.pins.push(index as u32);
                if let Some(class) = active_class(tab.agent_status, facts.voice, facts.subagents) {
                    self.pins_classes |= 1 << class;
                }
            }
        }
        // The Scheduled entries: by interval, then snapshot order.
        self.scheduled.clear();
        self.scheduled_kinds = 0;
        if sections.scheduled {
            for (index, (tab, facts)) in snapshot.tabs.iter().zip(&self.tabs).enumerate() {
                if facts.fixed {
                    continue;
                }
                let Some((kind, _)) = tab.remind_every.and_then(interval_rank) else {
                    continue;
                };
                self.scheduled.push(index as u32);
                self.scheduled_kinds |= 1 << kind;
            }
            let tabs = &snapshot.tabs;
            self.scheduled.sort_by_key(|index| {
                let rank = tabs
                    .get(*index as usize)
                    .and_then(|tab| tab.remind_every)
                    .and_then(interval_rank)
                    .map_or(u8::MAX, |(_, rank)| rank);
                (rank, *index)
            });
        }
    }

    /// Fork: a group's tab rows with every run of `MIN_RUN` or more
    /// foldable tabs in a row (`run_member`) as a `Row::Run`, followed by
    /// its tab rows while expanded.
    fn push_tab_rows(
        &mut self,
        snapshot: &ClientShellSnapshot,
        tabs: &[u32],
        expanded_runs: Option<&HashSet<String>>,
    ) {
        let mut index = 0;
        while index < tabs.len() {
            let run = tabs[index..]
                .iter()
                .take_while(|tab| self.run_member(snapshot, **tab))
                .count();
            if run < MIN_RUN {
                // Not a run: this tab (and any short run) as plain rows.
                let plain = run.max(1);
                self.rows.extend(
                    tabs[index..index + plain]
                        .iter()
                        .map(|tab| Row::Tab { tab: *tab }),
                );
                index += plain;
                continue;
            }
            let members = &tabs[index..index + run];
            let start = u32::try_from(self.run_tabs.len()).unwrap_or(u32::MAX);
            self.run_tabs.extend_from_slice(members);
            let expanded = expanded_runs.is_some_and(|runs| {
                snapshot
                    .tabs
                    .get(members[0] as usize)
                    .is_some_and(|tab| runs.contains(&tab.tab_id))
            });
            self.rows.push(Row::Run {
                start,
                len: u16::try_from(run).unwrap_or(u16::MAX),
                expanded,
            });
            if expanded {
                self.rows
                    .extend(members.iter().map(|tab| Row::Tab { tab: *tab }));
            }
            index += run;
        }
    }

    /// Fork: `snapshot.tabs[tab]` may fold into a suspended run: it is
    /// suspended and not focused, pinned, important or scheduled.
    fn run_member(&self, snapshot: &ClientShellSnapshot, tab: u32) -> bool {
        let (Some(info), Some(facts)) =
            (snapshot.tabs.get(tab as usize), self.tabs.get(tab as usize))
        else {
            return false;
        };
        info.agent_status == AgentStatus::Suspended
            && !info.focused
            && !info.important
            && info.remind_every.is_none()
            && !facts.pin
    }

    /// Fork: the tabs of the run `Row::Run { start, len, .. }`.
    pub(crate) fn run(&self, start: u32, len: u16) -> &[u32] {
        let start = start as usize;
        self.run_tabs
            .get(start..start.saturating_add(usize::from(len)))
            .unwrap_or(&[])
    }

    /// The facts of `snapshot.tabs[tab]`.
    pub(crate) fn tab(&self, tab: u32) -> Option<&TabFacts> {
        self.tabs.get(tab as usize)
    }
}

/// Active classes, in display order.
pub(crate) const CLASS_BLOCKED: u8 = 0;
pub(crate) const CLASS_VOICE: u8 = 1;
pub(crate) const CLASS_WORKING: u8 = 2;
/// An idle tab whose agents run subagents (background work reads as idle).
pub(crate) const CLASS_SUBAGENTS: u8 = 3;
pub(crate) const CLASS_DONE: u8 = 4;

/// A tab's Active class: blocked, live voice (`Unknown` counts as live, as
/// `voice::voice_mark` draws it), working, idle with subagents running
/// (`subagents` as `TabFacts::subagents` counts them), finished; `None` for
/// an idle tab without subagents or live voice, and for an unknown or
/// suspended one.
pub(crate) fn active_class(
    status: AgentStatus,
    voice: Option<AgentVoiceMode>,
    subagents: u32,
) -> Option<u8> {
    if status == AgentStatus::Blocked {
        return Some(CLASS_BLOCKED);
    }
    if matches!(voice, Some(AgentVoiceMode::Live | AgentVoiceMode::Unknown)) {
        return Some(CLASS_VOICE);
    }
    match status {
        AgentStatus::Working => Some(CLASS_WORKING),
        AgentStatus::Idle if subagents > 0 => Some(CLASS_SUBAGENTS),
        AgentStatus::Done => Some(CLASS_DONE),
        _ => None,
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
            .filter(|index| model.tab(*index as u32).is_some_and(|facts| !facts.fixed))
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
        Some(SidebarHover::Fixed(kind)) => Some(Selected::Fixed(*kind)),
        Some(SidebarHover::PinsEntry(tab_id)) => list_tab(tab_id).map(Selected::PinsEntry),
        Some(SidebarHover::PinsHeader | SidebarHover::PinsMore) => Some(Selected::PinsHeader),
        Some(SidebarHover::ScheduledEntry(tab_id)) => {
            list_tab(tab_id).map(Selected::ScheduledEntry)
        }
        Some(SidebarHover::ScheduledHeader | SidebarHover::ScheduledMore) => {
            Some(Selected::ScheduledHeader)
        }
        Some(SidebarHover::Run(tab_id)) => model.rows.iter().find_map(|row| match *row {
            Row::Run { start, len, .. }
                if model
                    .run(start, len)
                    .first()
                    .and_then(|tab| snapshot.tabs.get(*tab as usize))
                    .is_some_and(|tab| tab.tab_id == *tab_id) =>
            {
                Some(Selected::Run { start, len })
            }
            _ => None,
        }),
        None => None,
    };
    hovered.unwrap_or_else(|| model.focused_tab.map_or(Selected::None, Selected::Tab))
}

/// A time in state at minute granularity, at most 5 cells: `<1m`, `{m}m`,
/// `{h}h{mm}`, `{d}d{h}h` under 10 days, else `{d}d`.
pub(crate) fn format_age(age: std::time::Duration) -> StackStr<8> {
    use std::fmt::Write as _;
    let minutes = age.as_secs() / 60;
    let (hours, days) = (minutes / 60, minutes / (60 * 24));
    let mut text = StackStr::new();
    // Every form fits the buffer (`9d23h`, at most 6 digits of days).
    let _ = if minutes == 0 {
        text.write_str("<1m")
    } else if minutes < 60 {
        write!(text, "{minutes}m")
    } else if hours < 24 {
        write!(text, "{hours}h{:02}", minutes % 60)
    } else if days < 10 {
        write!(text, "{days}d{}h", hours % 24)
    } else {
        write!(text, "{}d", days.min(999_999))
    };
    text
}

/// A countdown at minute granularity, rounded up ("in 4m" until 3m00s are
/// left, never "in <1m"): `in {m}m`, `in {h}h{mm}`, `in {d}d{h}h`. At most
/// 9 cells.
pub(crate) fn format_countdown(remaining: std::time::Duration) -> StackStr<12> {
    use std::fmt::Write as _;
    let minutes = countdown_minutes(remaining);
    let age = format_age(std::time::Duration::from_secs(minutes.saturating_mul(60)));
    let mut text = StackStr::new();
    let _ = write!(text, "in {}", age.as_str());
    text
}

/// Whole minutes left, rounded up (at least 1).
fn countdown_minutes(remaining: std::time::Duration) -> u64 {
    u64::try_from(remaining.as_millis().div_ceil(60_000))
        .unwrap_or(u64::MAX)
        .max(1)
}

/// When a countdown to `next_fire` next changes its text at `now`: the
/// instant its rounded-up minutes drop by one (`next_fire` itself in the
/// last minute). `None` once `next_fire` is reached (the reminder's own
/// tick repaints then).
pub(crate) fn next_countdown_tick(next_fire: Instant, now: Instant) -> Option<Instant> {
    let remaining = next_fire.checked_duration_since(now)?;
    if remaining.is_zero() {
        return None;
    }
    let minutes = countdown_minutes(remaining);
    Some(
        next_fire
            .checked_sub(std::time::Duration::from_secs((minutes - 1) * 60))
            .unwrap_or(next_fire),
    )
}

/// When a time in state since `since` next changes its text at `now`: the
/// next whole minute of its age.
pub(crate) fn next_age_tick(since: Instant, now: Instant) -> Instant {
    let minutes = now.saturating_duration_since(since).as_secs() / 60;
    since + std::time::Duration::from_secs((minutes + 1) * 60)
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
    fn ages_format_at_minute_granularity_in_five_cells() {
        let age = |secs: u64| format_age(std::time::Duration::from_secs(secs));
        assert_eq!(age(0).as_str(), "<1m");
        assert_eq!(age(59).as_str(), "<1m");
        assert_eq!(age(60).as_str(), "1m");
        assert_eq!(age(59 * 60 + 59).as_str(), "59m");
        assert_eq!(age(3600 + 5 * 60).as_str(), "1h05");
        assert_eq!(age(23 * 3600 + 59 * 60).as_str(), "23h59");
        assert_eq!(age(24 * 3600).as_str(), "1d0h");
        assert_eq!(age(9 * 86_400 + 23 * 3600).as_str(), "9d23h");
        assert_eq!(age(10 * 86_400).as_str(), "10d");
        assert_eq!(age(u64::MAX).as_str(), "999999d");
        for secs in [0, 61, 7_000, 90_000, 900_000, 90_000_000] {
            assert!(age(secs).as_str().len() <= 7);
        }
        let since = Instant::now();
        assert_eq!(
            next_age_tick(since, since + std::time::Duration::from_secs(125)),
            since + std::time::Duration::from_secs(180)
        );
        assert_eq!(
            next_age_tick(since + std::time::Duration::from_secs(5), since),
            since + std::time::Duration::from_secs(65),
            "a clock behind a stale since ticks a minute after it"
        );
    }

    #[test]
    fn active_classes_put_blocked_then_live_voice_then_working_then_subagents_then_done() {
        assert_eq!(
            active_class(AgentStatus::Blocked, Some(AgentVoiceMode::Live), 2),
            Some(CLASS_BLOCKED)
        );
        assert_eq!(
            active_class(AgentStatus::Idle, Some(AgentVoiceMode::Live), 2),
            Some(CLASS_VOICE)
        );
        assert_eq!(
            active_class(AgentStatus::Idle, Some(AgentVoiceMode::Unknown), 0),
            Some(CLASS_VOICE)
        );
        assert_eq!(
            active_class(AgentStatus::Idle, Some(AgentVoiceMode::Muted), 0),
            None
        );
        assert_eq!(
            active_class(AgentStatus::Working, Some(AgentVoiceMode::Muted), 0),
            Some(CLASS_WORKING)
        );
        assert_eq!(
            active_class(AgentStatus::Working, None, 3),
            Some(CLASS_WORKING)
        );
        assert_eq!(
            active_class(AgentStatus::Idle, Some(AgentVoiceMode::Muted), 1),
            Some(CLASS_SUBAGENTS)
        );
        assert_eq!(active_class(AgentStatus::Idle, None, 0), None);
        assert_eq!(active_class(AgentStatus::Done, None, 0), Some(CLASS_DONE));
        assert_eq!(
            active_class(AgentStatus::Done, None, 2),
            Some(CLASS_DONE),
            "a finished turn stays done"
        );
        assert_eq!(active_class(AgentStatus::Suspended, None, 2), None);
        assert_eq!(active_class(AgentStatus::Unknown, None, 2), None);
        const {
            assert!(
                CLASS_BLOCKED < CLASS_VOICE
                    && CLASS_VOICE < CLASS_WORKING
                    && CLASS_WORKING < CLASS_SUBAGENTS
                    && CLASS_SUBAGENTS < CLASS_DONE
            );
        }
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
