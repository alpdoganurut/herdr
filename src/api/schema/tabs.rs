use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub focus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TabListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabRenameParams {
    pub tab_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabMoveParams {
    pub tab_id: String,
    pub insert_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub agent_status: AgentStatus,
    /// The tab's color tag; absent when the tab has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<TabColor>,
    /// Marked important (`tab.set_reminder`, `tab.set_remind`): clients remind
    /// while its agent sits finished (unseen) or blocked. Absent when not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub important: bool,
    /// The tab's scheduled reminder (`tab.set_reminder`); absent when off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_every: Option<TabRemindInterval>,
    /// Fork: pinned (`tab.set_pinned`): listed in clients' Pinned section.
    /// Absent when not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// A named color tag on a tab. Clients map each name to a theme color.
/// `Unknown` is the fallback for a name this build does not know, so a newer
/// peer's color decodes (and is ignored) instead of failing the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TabColor {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
    #[serde(other)]
    Unknown,
}

impl TabColor {
    /// The settable colors, in picker and cycle order.
    pub const ALL: [TabColor; 7] = [
        TabColor::Red,
        TabColor::Orange,
        TabColor::Yellow,
        TabColor::Green,
        TabColor::Cyan,
        TabColor::Blue,
        TabColor::Purple,
    ];

    /// The colors the picker and the cycle key offer, chosen to stay
    /// distinct on 16-color terminal themes (where orange and yellow, or
    /// cyan and green, render alike). Every `ALL` color is still accepted
    /// by `tab.set_color` and drawn when set.
    pub const OFFERED: [TabColor; 4] = [
        TabColor::Red,
        TabColor::Yellow,
        TabColor::Green,
        TabColor::Purple,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TabColor::Red => "red",
            TabColor::Orange => "orange",
            TabColor::Yellow => "yellow",
            TabColor::Green => "green",
            TabColor::Cyan => "cyan",
            TabColor::Blue => "blue",
            TabColor::Purple => "purple",
            TabColor::Unknown => "unknown",
        }
    }

    /// A settable color by name (`Unknown` is never parsed).
    pub fn from_name(name: &str) -> Option<TabColor> {
        TabColor::ALL
            .into_iter()
            .find(|color| color.name().eq_ignore_ascii_case(name))
    }

    /// The next step of the cycle none -> red -> yellow -> green -> purple ->
    /// none over `OFFERED`. A color outside it (set earlier, through the API,
    /// or unknown) restarts the cycle at the first offered color.
    pub fn cycle_next(current: Option<TabColor>) -> Option<TabColor> {
        let Some(color) = current else {
            return Some(TabColor::OFFERED[0]);
        };
        match TabColor::OFFERED.iter().position(|c| *c == color) {
            Some(index) => TabColor::OFFERED.get(index + 1).copied(),
            None => Some(TabColor::OFFERED[0]),
        }
    }
}

/// Set or clear (`color: null`) a tab's color tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabSetColorParams {
    pub tab_id: String,
    #[serde(default)]
    pub color: Option<TabColor>,
}

/// Mark (`remind: true`) or unmark a tab as important: clients remind about
/// an important tab whose agent waits unseen. The boolean form of
/// `tab.set_reminder`'s `important`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabSetRemindParams {
    pub tab_id: String,
    pub remind: bool,
}

/// How often clients remind about a tab regardless of its agent: every 5, 10
/// or 30 minutes, every 1 or 6 hours, or daily at `ui.daily_reminder_time`.
/// `Unknown` is the fallback for an interval this build does not know; it is
/// never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum TabRemindInterval {
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "10m")]
    M10,
    #[serde(rename = "30m")]
    M30,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "6h")]
    H6,
    #[serde(rename = "daily")]
    Daily,
    #[serde(other)]
    Unknown,
}

impl TabRemindInterval {
    /// The settable intervals, in menu order.
    pub const ALL: [TabRemindInterval; 6] = [
        TabRemindInterval::M5,
        TabRemindInterval::M10,
        TabRemindInterval::M30,
        TabRemindInterval::H1,
        TabRemindInterval::H6,
        TabRemindInterval::Daily,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TabRemindInterval::M5 => "5m",
            TabRemindInterval::M10 => "10m",
            TabRemindInterval::M30 => "30m",
            TabRemindInterval::H1 => "1h",
            TabRemindInterval::H6 => "6h",
            TabRemindInterval::Daily => "daily",
            TabRemindInterval::Unknown => "unknown",
        }
    }

    /// The fixed period; `None` for `Daily` (a time of day) and `Unknown`.
    pub fn period(self) -> Option<std::time::Duration> {
        let minutes = match self {
            TabRemindInterval::M5 => 5,
            TabRemindInterval::M10 => 10,
            TabRemindInterval::M30 => 30,
            TabRemindInterval::H1 => 60,
            TabRemindInterval::H6 => 360,
            TabRemindInterval::Daily | TabRemindInterval::Unknown => return None,
        };
        Some(std::time::Duration::from_secs(minutes * 60))
    }
}

/// A scheduled reminder as set through `tab.set_reminder`: off or an
/// interval. Closed: an unknown name rejects the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum TabRemindEvery {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "10m")]
    M10,
    #[serde(rename = "30m")]
    M30,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "6h")]
    H6,
    #[serde(rename = "daily")]
    Daily,
}

impl TabRemindEvery {
    pub fn interval(self) -> Option<TabRemindInterval> {
        match self {
            TabRemindEvery::Off => None,
            TabRemindEvery::M5 => Some(TabRemindInterval::M5),
            TabRemindEvery::M10 => Some(TabRemindInterval::M10),
            TabRemindEvery::M30 => Some(TabRemindInterval::M30),
            TabRemindEvery::H1 => Some(TabRemindInterval::H1),
            TabRemindEvery::H6 => Some(TabRemindInterval::H6),
            TabRemindEvery::Daily => Some(TabRemindInterval::Daily),
        }
    }

    pub fn from_interval(interval: Option<TabRemindInterval>) -> TabRemindEvery {
        match interval {
            Some(TabRemindInterval::M5) => TabRemindEvery::M5,
            Some(TabRemindInterval::M10) => TabRemindEvery::M10,
            Some(TabRemindInterval::M30) => TabRemindEvery::M30,
            Some(TabRemindInterval::H1) => TabRemindEvery::H1,
            Some(TabRemindInterval::H6) => TabRemindEvery::H6,
            Some(TabRemindInterval::Daily) => TabRemindEvery::Daily,
            None | Some(TabRemindInterval::Unknown) => TabRemindEvery::Off,
        }
    }

    /// `off` or an interval by name (`TabRemindInterval::name`).
    pub fn from_name(name: &str) -> Option<TabRemindEvery> {
        if name.eq_ignore_ascii_case("off") {
            return Some(TabRemindEvery::Off);
        }
        TabRemindInterval::ALL
            .into_iter()
            .find(|interval| interval.name().eq_ignore_ascii_case(name))
            .map(|interval| TabRemindEvery::from_interval(Some(interval)))
    }
}

/// Change a tab's reminders: `important` and/or the scheduled reminder
/// `every`. An absent field leaves that reminder as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabSetReminderParams {
    pub tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub important: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<TabRemindEvery>,
}

/// Fork: pin or unpin a tab (`tab.set_pinned`). A pinned tab is listed in
/// clients' Pinned section and stays in its group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabSetPinnedParams {
    pub tab_id: String,
    pub pinned: bool,
}
