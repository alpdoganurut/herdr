//! The activity overlay (fork): what herdr tells the sidecar to show while an
//! agent works on a tab — the glow frame and the cursor injected into the
//! page, and the per-pane tab group the companion extension keeps. The
//! directive rides on every sidecar request of an agent's call
//! (`HostRequest.activity`); the sidecar does the drawing.

use serde_json::{json, Value};

use crate::api::schema::BrowserActor;
use crate::config::BrowserConfig;

/// Chrome's tab group colours herdr uses (grey left out: it reads as idle).
pub const GROUP_COLORS: [&str; 8] = [
    "blue", "red", "yellow", "green", "pink", "purple", "cyan", "orange",
];

/// A stable colour per pane id (FNV-1a over the id, so it survives restarts).
pub fn group_color(pane_id: &str) -> &'static str {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in pane_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    GROUP_COLORS[(hash % GROUP_COLORS.len() as u64) as usize]
}

/// `<agent label> · <herdr tab label>`, whichever exist; the pane id when neither does.
pub fn group_title(agent: Option<&str>, tab_label: &str, pane_id: &str) -> String {
    let agent = agent.map(str::trim).filter(|a| !a.is_empty());
    let tab = Some(tab_label.trim()).filter(|t| !t.is_empty());
    match (agent, tab) {
        (Some(agent), Some(tab)) => format!("{agent} · {tab}"),
        (Some(one), None) | (None, Some(one)) => one.to_string(),
        (None, None) => pane_id.to_string(),
    }
}

/// The directive for a call: only a pane's calls draw anything (the user's
/// and external callers' never touch the page), and only while
/// `[browser] show_activity` is on.
pub fn directive(actor: &BrowserActor, config: &BrowserConfig) -> Option<Value> {
    if !config.show_activity {
        return None;
    }
    let BrowserActor::Pane {
        pane_id,
        tab_label,
        agent,
        ..
    } = actor
    else {
        return None;
    };
    Some(json!({
        "frame": true,
        "animate": true,
        "group": {
            "key": pane_id,
            "title": group_title(agent.as_deref(), tab_label, pane_id),
            "color": group_color(pane_id),
            "collapse_ms": config.active_glyph_secs.saturating_mul(1000),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(agent: Option<&str>, label: &str) -> BrowserActor {
        BrowserActor::Pane {
            pane_id: "w2:p7".into(),
            tab_id: "w2:t3".into(),
            workspace_id: "w2".into(),
            tab_label: label.into(),
            workspace_label: None,
            agent: agent.map(str::to_string),
            session: "default".into(),
            gone: false,
        }
    }

    #[test]
    fn colours_are_stable_and_from_the_palette() {
        assert_eq!(group_color("w2:p7"), group_color("w2:p7"));
        assert!(GROUP_COLORS.contains(&group_color("w2:p7")));
        assert!(!GROUP_COLORS.contains(&"grey"));
        let distinct: std::collections::HashSet<&str> =
            (0..64).map(|i| group_color(&format!("w1:p{i}"))).collect();
        assert!(distinct.len() >= 6, "{distinct:?}");
    }

    #[test]
    fn titles_fall_back_from_agent_and_tab_to_the_pane_id() {
        assert_eq!(
            group_title(Some("claude"), "planner", "w2:p7"),
            "claude · planner"
        );
        assert_eq!(group_title(None, "planner", "w2:p7"), "planner");
        assert_eq!(group_title(Some("claude"), "  ", "w2:p7"), "claude");
        assert_eq!(group_title(None, "", "w2:p7"), "w2:p7");
    }

    #[test]
    fn only_a_pane_gets_a_directive_and_only_while_enabled() {
        let config = BrowserConfig {
            active_glyph_secs: 90,
            ..BrowserConfig::default()
        };
        let d = directive(&pane(Some("claude"), "planner"), &config).unwrap();
        assert_eq!(d["frame"], true);
        assert_eq!(d["animate"], true);
        assert_eq!(d["group"]["key"], "w2:p7");
        assert_eq!(d["group"]["title"], "claude · planner");
        assert_eq!(d["group"]["color"], group_color("w2:p7"));
        assert_eq!(d["group"]["collapse_ms"], 90_000);
        assert!(directive(&BrowserActor::User, &config).is_none());
        assert!(directive(&BrowserActor::External { raw: None }, &config).is_none());
        let off = BrowserConfig {
            show_activity: false,
            ..BrowserConfig::default()
        };
        assert!(directive(&pane(Some("claude"), "planner"), &off).is_none());
    }
}
