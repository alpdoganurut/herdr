//! The machine row (fork, machine tabs): one row at the very top of the
//! expanded `tabs` sidebar, `● Local 3  ○ Build`, and a thin rule under it,
//! while at least two machines are enabled. Per machine: its state's glyph,
//! its label and the number of agents behind the glyph. The machine shown is
//! bold and underlined; a click on another switches to it (`hits.machines`).
//!
//! The counts are taken once per snapshot (`MachineAgents`), so drawing the
//! row scans no agents, and a snapshot of a machine that is not shown
//! repaints only when it changes that machine's mark
//! (`machine_repaint_basis`).

use std::fmt::Write as _;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use super::machine_diagnostics::MachineDiagnostics;
use super::render::{display_width, put_text, put_truncated, ShellRenderState};
use super::sidebar_model::{SidebarHover, StackStr};
use super::*;

/// A machine being (re)connected.
const CONNECTING_GLYPH: &str = "\u{21BB}"; // ↻
/// A machine that needs the user (a login, an error).
const ATTENTION_GLYPH: &str = "!";
/// The cells left of the first and right of the last machine.
const MARGIN: u16 = 1;
/// A shortened label keeps its first letter and `…`.
const MIN_LABEL: u16 = 2;
/// Counts past this show as `99+`.
const MAX_COUNT: u32 = 99;

/// The agents of one snapshot the machine row counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MachineAgents {
    blocked: u32,
    working: u32,
}

impl MachineAgents {
    pub(super) fn of(snapshot: &ClientShellSnapshot) -> Self {
        let mut agents = Self::default();
        for agent in &snapshot.agents {
            match agent.agent_status {
                crate::api::schema::AgentStatus::Blocked => agents.blocked += 1,
                crate::api::schema::AgentStatus::Working => agents.working += 1,
                _ => {}
            }
        }
        agents
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MachineState {
    /// An agent waits on the user.
    Blocked,
    Working,
    Idle,
    Connecting,
    Attention,
}

/// What the row says about one machine beside its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MachineMark {
    state: MachineState,
    /// Agents behind the state's glyph (0: no count).
    count: u32,
}

impl MachineMark {
    pub(super) fn of(endpoint: &ClientShellEndpoint, diagnostics: &MachineDiagnostics) -> Self {
        let agents = endpoint.machine_agents;
        let (state, count) = match endpoint.status {
            ClientEndpointStatus::Attention => (MachineState::Attention, 0),
            _ if diagnostics.required_for(endpoint) => (MachineState::Attention, 0),
            ClientEndpointStatus::Online if endpoint.snapshot.is_some() => {
                if agents.blocked > 0 {
                    (MachineState::Blocked, agents.blocked)
                } else if agents.working > 0 {
                    (MachineState::Working, agents.working)
                } else {
                    (MachineState::Idle, 0)
                }
            }
            _ => (MachineState::Connecting, 0),
        };
        Self { state, count }
    }

    /// The glyph and its color, as the sidebar draws an agent in that state.
    pub(super) fn glyph(
        self,
        style: crate::config::StatusIndicatorStyle,
        palette: &Palette,
    ) -> (&'static str, Color) {
        use crate::api::schema::AgentStatus;
        match self.state {
            MachineState::Blocked => (
                status_icon(AgentStatus::Blocked, style),
                status_color(AgentStatus::Blocked, palette),
            ),
            MachineState::Working => (
                status_icon(AgentStatus::Working, style),
                status_color(AgentStatus::Working, palette),
            ),
            MachineState::Idle => (status_icon(AgentStatus::Idle, style), palette.overlay0),
            MachineState::Connecting => (CONNECTING_GLYPH, palette.overlay0),
            MachineState::Attention => (ATTENTION_GLYPH, palette.red),
        }
    }

    fn count_text(self) -> StackStr<8> {
        let mut text = StackStr::new();
        if self.count > MAX_COUNT {
            let _ = write!(text, " {MAX_COUNT}+");
        } else if self.count > 0 {
            let _ = write!(text, " {}", self.count);
        }
        text
    }
}

/// The machines the row lists: a disabled one is not connected on purpose.
fn listed(endpoint: &ClientShellEndpoint) -> bool {
    endpoint.status != ClientEndpointStatus::Disabled
}

/// Rows the tabs sidebar keeps at its very top: the machine row and its rule
/// while two machines or more are listed, else none.
pub(super) fn machine_rows(endpoints: &[ClientShellEndpoint]) -> u16 {
    if endpoints.iter().filter(|endpoint| listed(endpoint)).count() > 1 {
        2
    } else {
        0
    }
}

/// One machine's cells in the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    /// Index into the items.
    item: usize,
    /// Label cells (0: the glyph alone).
    label: u16,
    count: u16,
}

impl Segment {
    /// The glyph, a blank and the label, the count (with its own blank).
    fn width(&self) -> u16 {
        1 + self.label + u16::from(self.label > 0) + self.count
    }
}

/// The segments and the gap between them for items of (label cells, count
/// cells), `selected` one of them, in `width` cells. What does not fit gives
/// way in turn: the unselected labels (the longest first, to their first
/// letter and `…`), the unselected counts, the second gap cell, the selected
/// label (likewise), its count, the unselected labels whole, the selected
/// label whole. A glyph stays; with no room for every glyph the machines
/// farthest from the selected one go. What room is left then goes back to
/// the labels, the selected one's first.
fn fit(items: &[(u16, u16)], selected: usize, width: u16) -> (Vec<Segment>, u16) {
    let mut segments: Vec<Segment> = items
        .iter()
        .enumerate()
        .map(|(item, (label, count))| Segment {
            item,
            label: (*label).min(width),
            count: *count,
        })
        .collect();
    let mut gap = 2u16;
    let total = |segments: &[Segment], gap: u16| {
        segments
            .iter()
            .map(Segment::width)
            .sum::<u16>()
            .saturating_add(gap * (segments.len().saturating_sub(1) as u16))
    };
    while total(&segments, gap) > width {
        let Some(longest) = segments
            .iter_mut()
            .filter(|segment| segment.item != selected && segment.label > MIN_LABEL)
            .max_by_key(|segment| segment.label)
        else {
            break;
        };
        longest.label -= 1;
    }
    let others = |segments: &mut Vec<Segment>, change: fn(&mut Segment)| {
        segments
            .iter_mut()
            .filter(|segment| segment.item != selected)
            .for_each(change);
    };
    let own = |segments: &mut Vec<Segment>, change: fn(&mut Segment)| {
        segments
            .iter_mut()
            .filter(|segment| segment.item == selected)
            .for_each(change);
    };
    if total(&segments, gap) > width {
        others(&mut segments, |segment| segment.count = 0);
    }
    if total(&segments, gap) > width {
        gap = 1;
    }
    while total(&segments, gap) > width
        && segments
            .iter()
            .any(|segment| segment.item == selected && segment.label > MIN_LABEL)
    {
        own(&mut segments, |segment| segment.label -= 1);
    }
    if total(&segments, gap) > width {
        own(&mut segments, |segment| segment.count = 0);
    }
    if total(&segments, gap) > width {
        others(&mut segments, |segment| segment.label = 0);
    }
    if total(&segments, gap) > width {
        own(&mut segments, |segment| segment.label = 0);
    }
    while total(&segments, gap) > width && segments.len() > 1 {
        let at = segments
            .iter()
            .position(|segment| segment.item == selected)
            .unwrap_or(0);
        if at + 1 < segments.len() && segments.len() - 1 - at >= at {
            segments.pop();
        } else {
            segments.remove(0);
        }
    }
    // Back to the labels, one cell at a time (the first cell of a hidden one
    // costs its blank too).
    for own_turn in [true, false] {
        while let Some(index) = segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                (segment.item == selected) == own_turn
                    && segment.label > 0
                    && segment.label < items[segment.item].0
            })
            .min_by_key(|(_, segment)| segment.label)
            .map(|(index, _)| index)
        {
            if total(&segments, gap) >= width {
                break;
            }
            segments[index].label += 1;
        }
    }
    (segments, gap)
}

/// Draw the row into `row` and record its machines in `hits.machines`.
pub(super) fn render_machine_row(
    buffer: &mut Buffer,
    row: Rect,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    let machines: Vec<(&ClientShellEndpoint, MachineMark)> = state
        .endpoints
        .iter()
        .filter(|endpoint| listed(endpoint))
        .map(|endpoint| {
            (
                endpoint,
                MachineMark::of(endpoint, state.machine_diagnostics),
            )
        })
        .collect();
    let items: Vec<(u16, u16)> = machines
        .iter()
        .map(|(endpoint, mark)| {
            (
                display_width(&endpoint.label),
                display_width(mark.count_text().as_str()),
            )
        })
        .collect();
    let selected = machines
        .iter()
        .position(|(endpoint, _)| &endpoint.endpoint_id == state.active_endpoint_id)
        .unwrap_or(0);
    let (segments, gap) = fit(&items, selected, row.width.saturating_sub(2 * MARGIN));
    let mut x = row.x.saturating_add(MARGIN);
    for segment in segments {
        let (endpoint, mark) = machines[segment.item];
        let width = segment.width().min(row.right().saturating_sub(x));
        let rect = Rect::new(x, row.y, width, 1);
        let hovered = matches!(state.sidebar_hover, Some(SidebarHover::Machine(id)) if id == &endpoint.endpoint_id);
        if hovered {
            buffer.set_style(rect, Style::default().bg(palette.sidebar_hover_bg()));
        }
        let (glyph, color) = mark.glyph(config.status_indicators, palette);
        put_text(buffer, x, row.y, width, glyph, Style::default().fg(color));
        let mut end = x.saturating_add(1);
        if segment.label > 0 {
            let style = if segment.item == selected {
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else if hovered {
                Style::default().fg(palette.text)
            } else {
                Style::default().fg(palette.subtext0)
            };
            put_truncated(
                buffer,
                x.saturating_add(2),
                row.y,
                segment.label,
                &endpoint.label,
                style,
            );
            end = x.saturating_add(2 + segment.label);
        }
        if segment.count > 0 {
            put_text(
                buffer,
                end,
                row.y,
                segment.count,
                mark.count_text().as_str(),
                Style::default().fg(color),
            );
        }
        hits.machines.push(MachineHit {
            rect,
            status_badge: Rect::default(),
            collapse_toggle: Rect::default(),
            endpoint_id: endpoint.endpoint_id.clone(),
        });
        x = x.saturating_add(width).saturating_add(gap);
    }
}

/// What a machine that is not shown had on screen before its snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineRepaintBasis {
    mark: MachineMark,
    /// Agent cards show only for the server that sent them.
    boot_id: Option<String>,
    notifications: usize,
}

impl ClientShellState {
    /// What a snapshot of `endpoint_id` can change on screen when only the
    /// machine row (and the sticky toasts) show that machine; `None` when
    /// more of it may show: the active machine, the phone and `spaces`
    /// layouts and the collapsed rail (they list every machine), an overlay.
    pub(crate) fn machine_repaint_basis(
        &self,
        endpoint_id: &ClientEndpointId,
    ) -> Option<MachineRepaintBasis> {
        if endpoint_id == &self.active_endpoint_id
            || self.overlay.is_some()
            || !self.machine_sidebar_shown()
        {
            return None;
        }
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)?;
        Some(MachineRepaintBasis {
            mark: MachineMark::of(endpoint, &self.machine_diagnostics),
            boot_id: endpoint
                .snapshot
                .as_deref()
                .map(|snapshot| snapshot.boot_id.clone()),
            notifications: self.visible_notifications.len(),
        })
    }

    /// Whether a snapshot of `endpoint_id` that came after `before` needs a
    /// frame.
    pub(crate) fn machine_repaint_due(
        &self,
        endpoint_id: &ClientEndpointId,
        before: Option<MachineRepaintBasis>,
    ) -> bool {
        before.is_none() || before != self.machine_repaint_basis(endpoint_id)
    }

    /// A left press on a machine of the row: an online one is shown (the one
    /// shown already stays, and a switch away that is under way is called
    /// off), any other is asked to reconnect now (`select_unready_machine`).
    pub(super) fn machine_row_press(
        &mut self,
        endpoint_id: ClientEndpointId,
        outcome: &mut ClientShellInput,
    ) {
        if endpoint_id.is_local() || self.endpoint_is_online(&endpoint_id) {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id,
                target: None,
            });
        } else {
            self.select_unready_machine(&endpoint_id, outcome);
        }
    }

    /// `keys.next_machine` / `keys.previous_machine`: the next online machine
    /// in the row's order, wrapping; nothing without another one.
    pub(super) fn step_machine(&mut self, forward: bool, outcome: &mut ClientShellInput) {
        let count = self.endpoints.len();
        let Some(current) = self
            .endpoints
            .iter()
            .position(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        else {
            return;
        };
        let target = (1..count)
            .map(|step| {
                if forward {
                    (current + step) % count
                } else {
                    (current + count - step) % count
                }
            })
            .map(|index| self.endpoints[index].endpoint_id.clone())
            .find(|endpoint_id| self.endpoint_is_online(endpoint_id));
        if let Some(endpoint_id) = target {
            self.activate_endpoint(endpoint_id, outcome);
        }
    }
}

#[cfg(test)]
impl ClientShellState {
    pub(crate) fn last_composed(&self) -> Option<std::time::Instant> {
        self.last_composed_at
    }

    pub(crate) fn endpoint_notice_visible(&self) -> bool {
        self.visible_endpoint_notice.is_some()
    }

    /// One agent in `status` and nothing else.
    pub(crate) fn test_one_agent_snapshot(
        boot_id: &str,
        revision: u64,
        status: crate::api::schema::AgentStatus,
    ) -> ClientShellSnapshot {
        let mut snapshot = super::tests::snapshot();
        snapshot.boot_id = boot_id.into();
        snapshot.revision = revision;
        snapshot.agents = vec![crate::protocol::ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: None,
            display_agent: None,
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: revision,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
            subagents: 0,
        }];
        snapshot
    }

    pub(crate) fn set_test_one_agent_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        boot_id: &str,
        revision: u64,
        status: crate::api::schema::AgentStatus,
    ) {
        self.set_endpoint_snapshot(
            endpoint_id,
            Box::new(Self::test_one_agent_snapshot(boot_id, revision, status)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every width, the selected one and the gap; the invariants of `fit`.
    fn fitted(items: &[(u16, u16)], selected: usize, width: u16) -> (Vec<Segment>, u16) {
        let (segments, gap) = fit(items, selected, width);
        assert!(
            segments.iter().any(|segment| segment.item == selected),
            "{items:?} {selected} {width}"
        );
        assert!(
            segments
                .windows(2)
                .all(|pair| pair[0].item + 1 == pair[1].item),
            "neighbours, in order"
        );
        let total =
            segments.iter().map(Segment::width).sum::<u16>() + gap * (segments.len() as u16 - 1);
        assert!(total <= width.max(1), "{items:?} {selected} {width}");
        for segment in &segments {
            assert!(segment.label <= items[segment.item].0);
        }
        (segments, gap)
    }

    #[test]
    fn every_width_and_number_of_machines_fits() {
        for machines in 1..=9usize {
            let items: Vec<(u16, u16)> = (0..machines)
                .map(|index| (3 + (index as u16 * 5) % 19, 2 * (index as u16 % 2)))
                .collect();
            for selected in 0..machines {
                for width in 0..=60 {
                    fitted(&items, selected, width);
                }
            }
        }
    }

    #[test]
    fn rows_give_way_in_the_documented_order() {
        // Local (5, count 2), buildbox (8, count 2); Local selected.
        let items = [(5, 2), (8, 2)];
        let natural = (1 + 1 + 5 + 2) + 2 + (1 + 1 + 8 + 2);
        let cells = |width| {
            let (segments, gap) = fitted(&items, 0, width);
            let pairs: Vec<(u16, u16)> = segments
                .iter()
                .map(|segment| (segment.label, segment.count))
                .collect();
            (pairs, gap)
        };
        assert_eq!(cells(natural), (vec![(5, 2), (8, 2)], 2));
        assert_eq!(cells(natural - 1), (vec![(5, 2), (7, 2)], 2));
        assert_eq!(cells(natural - 6), (vec![(5, 2), (2, 2)], 2));
        // The unselected count goes, its room back to its label.
        assert_eq!(cells(natural - 7), (vec![(5, 2), (3, 0)], 2));
        assert_eq!(cells(natural - 8), (vec![(5, 2), (2, 0)], 2));
        assert_eq!(cells(natural - 9), (vec![(5, 2), (2, 0)], 1));
        // Then the selected label, its glyph and count stay.
        assert_eq!(cells(natural - 10), (vec![(4, 2), (2, 0)], 1));
        assert_eq!(cells(natural - 12), (vec![(2, 2), (2, 0)], 1));
        // Its count, then the unselected label whole.
        assert_eq!(cells(natural - 13), (vec![(3, 0), (2, 0)], 1));
        assert_eq!(cells(natural - 14), (vec![(2, 0), (2, 0)], 1));
        assert_eq!(cells(natural - 15), (vec![(4, 0), (0, 0)], 1));
        // Both glyphs to the end; one when there is room for one.
        assert_eq!(cells(3), (vec![(0, 0), (0, 0)], 1));
        assert_eq!(cells(2).0.len(), 1);
    }

    #[test]
    fn the_selected_machine_stays_when_not_every_glyph_fits() {
        let items = [(4, 0); 9];
        for selected in 0..9 {
            let (segments, _) = fitted(&items, selected, 7);
            assert_eq!(segments.len(), 4, "four glyphs in seven cells");
        }
    }
}
