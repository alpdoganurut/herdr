//! The herdr+ new tab page's data (fork): a compact snapshot of what
//! `browser.get` knows — agents (panes) with their tabs, the user's own tabs,
//! the standing line — pushed to the companion extension's session storage
//! whenever the ledger changes (debounced) and on attach. The page renders it
//! with textContent only; nothing here is trusted page text made safe, it is
//! the same untrusted titles and URLs, so the page never uses innerHTML.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::api::schema::{BrowserActor, BrowserGetInfo};
use crate::config::BrowserConfig;

/// The schema version of the snapshot (the page ignores others).
pub const NTP_SNAPSHOT_VERSION: u32 = 1;

fn pane_of(actor: &BrowserActor) -> Option<&str> {
    actor.pane_id()
}

/// New-tab pages (Chromium's and the companion's own) are not worth a row.
fn is_new_tab_page(url: &str) -> bool {
    url.starts_with("chrome://newtab")
        || url.starts_with("chrome://new-tab-page")
        || (url.starts_with("chrome-extension://") && url.ends_with("/newtab.html"))
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let rest = rest.trim_start_matches("www.");
    rest.split(['/', '?', '#']).next().unwrap_or("").to_string()
}

/// Build the page's snapshot from the ledger view.
pub fn snapshot(info: &BrowserGetInfo, config: &BrowserConfig, now: u64) -> Value {
    struct Agent {
        label: String,
        agent: Option<String>,
        pane_id: String,
        tab_id: Option<String>,
        last_op: Option<String>,
        last_at: u64,
        current: Option<String>,
        tabs: Vec<Value>,
    }
    let window = config.active_glyph_secs;
    let mut agents: BTreeMap<String, Agent> = BTreeMap::new();
    let mut seed = |actor: &BrowserActor| {
        if let BrowserActor::Pane {
            pane_id,
            tab_id,
            tab_label,
            agent,
            ..
        } = actor
        {
            agents.entry(pane_id.clone()).or_insert_with(|| Agent {
                label: if tab_label.trim().is_empty() {
                    pane_id.clone()
                } else {
                    tab_label.trim().to_string()
                },
                agent: agent.clone(),
                pane_id: pane_id.clone(),
                tab_id: Some(tab_id.clone()).filter(|t| !t.is_empty()),
                last_op: None,
                last_at: 0,
                current: None,
                tabs: Vec::new(),
            });
        }
    };
    for tab in info.tabs.iter().filter(|tab| tab.state == "open") {
        seed(&tab.last_actor);
        seed(&tab.opened_by);
    }
    for cursor in &info.recent_panes {
        if let Some(agent) = agents.get_mut(&cursor.pane_id) {
            agent.current = Some(cursor.current.clone());
            agent.last_at = agent.last_at.max(cursor.last_at);
        }
    }
    let mut yours: Vec<Value> = Vec::new();
    let mut last_at_all = 0u64;
    for tab in info
        .tabs
        .iter()
        .filter(|tab| tab.state == "open" && !is_new_tab_page(&tab.url))
    {
        let touched = tab.last.as_ref().map(|t| t.at).unwrap_or(0);
        last_at_all = last_at_all.max(touched);
        let (short, profile) = match tab.id.split_once(':') {
            Some((profile, short)) => (short.to_string(), profile.to_string()),
            None => (tab.id.clone(), tab.profile.clone()),
        };
        let row = json!({
            "id": tab.id,
            "short": short,
            "profile": profile,
            "title": tab.title,
            "url": tab.url,
            "host": host_of(&tab.url),
            "last_at": touched,
            "selected": tab.selected,
        });
        let owner = pane_of(&tab.last_actor)
            .or_else(|| pane_of(&tab.opened_by))
            .map(str::to_string);
        match owner.and_then(|pane| agents.get_mut(&pane)) {
            Some(agent) => {
                if let Some(touch) = tab.last.as_ref() {
                    if touch.at >= agent.last_at && touch.actor.pane_id() == Some(&agent.pane_id) {
                        agent.last_at = touch.at;
                        agent.last_op = Some(touch.op.clone());
                    }
                }
                let mut row = row;
                row["current"] = json!(agent.current.as_deref() == Some(&tab.id));
                agent.tabs.push(row);
            }
            None => yours.push(row),
        }
    }
    let mut agent_rows: Vec<(u64, Value)> = agents
        .into_values()
        .filter(|agent| !agent.tabs.is_empty())
        .map(|agent| {
            let active = agent.last_at > 0 && now.saturating_sub(agent.last_at) <= window;
            let mut tabs = agent.tabs;
            tabs.sort_by(|a, b| b["last_at"].as_u64().cmp(&a["last_at"].as_u64()));
            (
                agent.last_at,
                json!({
                    "pane_id": agent.pane_id,
                    "label": agent.label,
                    "agent": agent.agent,
                    "symbol": config.group_symbol(agent.agent.as_deref()),
                    "herdr_tab": agent.tab_id,
                    "where": agent.pane_id.replace(':', " · "),
                    "last_op": agent.last_op,
                    "last_at": agent.last_at,
                    "active": active,
                    "current": agent.current,
                    "tabs": tabs,
                }),
            )
        })
        .collect();
    agent_rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    yours.sort_by(|a, b| b["last_at"].as_u64().cmp(&a["last_at"].as_u64()));
    let agent_tabs: usize = agent_rows
        .iter()
        .map(|(_, a)| a["tabs"].as_array().map(Vec::len).unwrap_or(0))
        .sum();
    let profile = info
        .profiles
        .iter()
        .find(|p| p.state == "running")
        .map(|p| p.name.clone())
        .unwrap_or_else(|| config.default_profile().to_string());
    json!({
        "version": NTP_SNAPSHOT_VERSION,
        "at": now,
        "show_activity": config.show_activity,
        "profile": profile,
        "agents": agent_rows.into_iter().map(|(_, a)| a).collect::<Vec<_>>(),
        "agent_tabs": agent_tabs,
        "yours": yours,
        "last_at": last_at_all,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{BrowserPaneCursor, BrowserProfileInfo, BrowserTabInfo, BrowserTouch};

    fn pane(id: &str, label: &str, agent: Option<&str>) -> BrowserActor {
        BrowserActor::Pane {
            pane_id: id.into(),
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

    fn tab(
        id: &str,
        url: &str,
        title: &str,
        opened_by: BrowserActor,
        last: Option<(BrowserActor, &str, u64)>,
    ) -> BrowserTabInfo {
        let last_actor = last
            .as_ref()
            .map(|(a, _, _)| a.clone())
            .unwrap_or_else(|| opened_by.clone());
        BrowserTabInfo {
            id: id.into(),
            profile: "main".into(),
            target_id: format!("T-{id}"),
            url: url.into(),
            title: title.into(),
            selected: false,
            opened_by,
            last: last.map(|(actor, op, at)| BrowserTouch {
                actor,
                op: op.into(),
                detail: String::new(),
                at,
                ok: true,
            }),
            last_actor,
            users: vec![],
            dialog_open: false,
            console_errors: 0,
            active: false,
            state: "open".into(),
            closed_at: None,
        }
    }

    #[test]
    fn agents_get_their_tabs_the_user_keeps_hers_and_the_standing_facts_add_up() {
        let planner = pane("w2:p3", "planner", Some("claude"));
        let research = pane("w1:p2", "research", Some("codex"));
        let info = BrowserGetInfo {
            seq: 9,
            unchanged: false,
            enabled: true,
            host: Default::default(),
            profiles: vec![BrowserProfileInfo {
                name: "main".into(),
                state: "running".into(),
                ..Default::default()
            }],
            tabs: vec![
                tab(
                    "main:t12",
                    "https://en.wikipedia.org/wiki/Chromium",
                    "Chromium — Wikipedia",
                    planner.clone(),
                    Some((planner.clone(), "act:click", 1000)),
                ),
                tab(
                    "main:t10",
                    "https://www.wikipedia.org/",
                    "Wikipedia",
                    planner.clone(),
                    Some((planner.clone(), "open", 970)),
                ),
                tab(
                    "main:t14",
                    "https://news.ycombinator.com/",
                    "Hacker News",
                    research.clone(),
                    Some((research.clone(), "read", 990)),
                ),
                tab(
                    "main:t4",
                    "https://eksisozluk.com/giris?x=1#top",
                    "giriş",
                    BrowserActor::User,
                    None,
                ),
                tab(
                    "main:t5",
                    "chrome://new-tab-page/",
                    "New Tab",
                    BrowserActor::User,
                    None,
                ),
                tab(
                    "main:t6",
                    "chrome-extension://jmegdddadjhcnkdfpmeeockadkdbfpop/newtab.html",
                    "New Tab",
                    BrowserActor::User,
                    None,
                ),
                {
                    let mut closed = tab(
                        "main:t2",
                        "https://gone.test/",
                        "gone",
                        planner.clone(),
                        None,
                    );
                    closed.state = "closed".into();
                    closed
                },
            ],
            recent_panes: vec![
                BrowserPaneCursor {
                    pane_id: "w2:p3".into(),
                    tab_id: Some("w2:t3".into()),
                    current: "main:t12".into(),
                    last_at: 1000,
                },
                BrowserPaneCursor {
                    pane_id: "w1:p2".into(),
                    tab_id: None,
                    current: "main:t14".into(),
                    last_at: 990,
                },
            ],
        };
        let config = BrowserConfig {
            active_glyph_secs: 60,
            ..BrowserConfig::default()
        };
        let snap = snapshot(&info, &config, 1004);
        assert_eq!(snap["version"], NTP_SNAPSHOT_VERSION);
        assert_eq!(snap["profile"], "main");
        assert_eq!(snap["agent_tabs"], 3);
        assert_eq!(snap["last_at"], 1000);
        let agents = snap["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 2);
        let first = &agents[0];
        assert_eq!(first["label"], "planner", "most recent agent first");
        assert_eq!(first["symbol"], "✻");
        assert_eq!(first["where"], "w2 · p3");
        assert_eq!(first["last_op"], "act:click");
        assert_eq!(first["last_at"], 1000);
        assert_eq!(first["active"], true);
        assert_eq!(first["current"], "main:t12");
        let tabs = first["tabs"].as_array().unwrap();
        assert_eq!(tabs.len(), 2, "closed tabs are left out");
        assert_eq!(tabs[0]["short"], "t12");
        assert_eq!(tabs[0]["current"], true);
        assert_eq!(tabs[0]["host"], "en.wikipedia.org");
        assert_eq!(tabs[1]["short"], "t10");
        assert_eq!(tabs[1]["current"], false);
        assert_eq!(tabs[1]["host"], "wikipedia.org", "www. dropped");
        let second = &agents[1];
        assert_eq!(second["symbol"], "◇");
        assert_eq!(second["label"], "research");
        let yours = snap["yours"].as_array().unwrap();
        assert_eq!(yours.len(), 1, "new-tab pages are not listed");
        assert_eq!(yours[0]["short"], "t4");
        assert_eq!(
            yours[0]["host"], "eksisozluk.com",
            "query and fragment dropped"
        );
        // idle after the window; off by config
        let later = snapshot(&info, &config, 2000);
        assert_eq!(later["agents"][0]["active"], false);
        let off = BrowserConfig {
            show_activity: false,
            ..config.clone()
        };
        assert_eq!(snapshot(&info, &off, 1004)["show_activity"], false);
        // nothing at all
        let empty = BrowserGetInfo {
            seq: 1,
            unchanged: false,
            enabled: true,
            host: Default::default(),
            profiles: vec![],
            tabs: vec![],
            recent_panes: vec![],
        };
        let snap = snapshot(&empty, &config, 5);
        assert_eq!(snap["agents"].as_array().unwrap().len(), 0);
        assert_eq!(snap["yours"].as_array().unwrap().len(), 0);
        assert_eq!(
            snap["profile"], "main",
            "the default profile when none runs"
        );
    }
}
