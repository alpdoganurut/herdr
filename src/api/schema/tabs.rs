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

    /// The next step of the cycle none -> red -> ... -> purple -> none. An
    /// unknown color restarts the cycle at the first color.
    pub fn cycle_next(current: Option<TabColor>) -> Option<TabColor> {
        match current {
            None | Some(TabColor::Unknown) => Some(TabColor::ALL[0]),
            Some(color) => {
                let index = TabColor::ALL.iter().position(|c| *c == color)?;
                TabColor::ALL.get(index + 1).copied()
            }
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
