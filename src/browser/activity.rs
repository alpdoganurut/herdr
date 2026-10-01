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

/// `<agent symbol> <herdr tab label>` (no provider names in the window); the
/// pane id stands in when the tab has no label.
pub fn group_title(symbol: &str, tab_label: &str, pane_id: &str) -> String {
    let label = Some(tab_label.trim())
        .filter(|t| !t.is_empty())
        .unwrap_or(pane_id);
    let symbol = symbol.trim();
    if symbol.is_empty() {
        label.to_string()
    } else {
        format!("{symbol} {label}")
    }
}

/// The directive for a call: only a pane's calls draw anything (the user's
/// and external callers' never touch the page), and only while
/// `[browser] show_activity` is on. `linger_ms` is how long the frame (and
/// the cursor where the last op left it) stays after the op — the
/// `active_glyph_secs` window, the same one the sidebar's ◎ and the group's
/// activity mark follow.
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
    let window_ms = config.active_glyph_secs.saturating_mul(1000);
    Some(json!({
        "frame": true,
        "animate": true,
        "color": config.activity_color(),
        "linger_ms": window_ms,
        "group": {
            "key": pane_id,
            "title": group_title(&config.group_symbol(agent.as_deref()), tab_label, pane_id),
            "color": group_color(pane_id),
            "collapse_ms": window_ms,
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
            shell_pid: None,
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
    fn titles_are_a_symbol_and_the_tab_label_with_the_pane_id_as_fallback() {
        assert_eq!(group_title("✻", "planner", "w2:p7"), "✻ planner");
        assert_eq!(group_title("◇", "  ", "w2:p7"), "◇ w2:p7");
        assert_eq!(group_title("", "planner", "w2:p7"), "planner");
        let config = BrowserConfig::default();
        assert_eq!(
            group_title(&config.group_symbol(Some("codex")), "review", "w1:p1"),
            "◇ review"
        );
        assert_eq!(
            group_title(&config.group_symbol(None), "shell", "w1:p1"),
            "◌ shell"
        );
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
        assert_eq!(d["color"], "#aa6eff", "the default overlay colour");
        let teal = BrowserConfig {
            activity_color: "#00C8FF".into(),
            ..BrowserConfig::default()
        };
        assert_eq!(
            directive(&pane(Some("claude"), "planner"), &teal).unwrap()["color"],
            "#00c8ff"
        );
        let bad = BrowserConfig {
            activity_color: "purple".into(),
            ..BrowserConfig::default()
        };
        assert_eq!(
            directive(&pane(Some("claude"), "planner"), &bad).unwrap()["color"],
            "#aa6eff",
            "an invalid colour falls back to the default"
        );
        assert_eq!(d["group"]["key"], "w2:p7");
        assert_eq!(
            d["group"]["title"], "✻ planner",
            "a symbol, never the provider name"
        );
        let custom = BrowserConfig {
            group_symbols: [("claude".to_string(), "C".to_string())]
                .into_iter()
                .collect(),
            ..BrowserConfig::default()
        };
        assert_eq!(
            directive(&pane(Some("claude"), "planner"), &custom).unwrap()["group"]["title"],
            "C planner"
        );
        assert_eq!(d["group"]["color"], group_color("w2:p7"));
        assert_eq!(d["group"]["collapse_ms"], 90_000);
        assert_eq!(
            d["linger_ms"], 90_000,
            "the frame's window is the active-glyph window"
        );
        assert!(directive(&BrowserActor::User, &config).is_none());
        assert!(directive(&BrowserActor::External { raw: None }, &config).is_none());
        let off = BrowserConfig {
            show_activity: false,
            ..BrowserConfig::default()
        };
        assert!(directive(&pane(Some("claude"), "planner"), &off).is_none());
    }
}
