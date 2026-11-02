//! Per-herdr-tab browser state (fork, companion browser): the group of
//! pages a herdr tab (a session) has in the companion browser. A shared
//! runtime fact: it lives on `Tab.browser`, persists in
//! `TabSnapshot.browser`, travels with whole-tab moves and live handoff.
//!
//! The companion owns the live list and replaces it per group with
//! `browser.host_sync`; herdr mints the stable group key and entry ids and
//! keeps the list as the restore source. Pure data: no hub, no sidecar.

use std::collections::hash_map::RandomState;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::api::schema::{BrowserHostEntry, BrowserHostSyncGroup, TabBrowserKind};

/// The most entries one herdr tab keeps (the rest of a sync is dropped).
pub const MAX_ENTRIES: usize = 32;
/// Longer URLs are truncated, never refused.
pub const MAX_URL_CHARS: usize = 2048;
/// Longer titles are truncated, never refused.
pub const MAX_TITLE_CHARS: usize = 300;

/// A herdr tab's browser group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TabBrowser {
    /// `g-` + 16 hex, minted once, stable across moves and restarts.
    pub key: String,
    /// Strip order.
    #[serde(default)]
    pub entries: Vec<TabBrowserEntry>,
    /// The active entry's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Bumped on every change (runtime only).
    #[serde(skip)]
    pub rev: u64,
}

/// One page of a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TabBrowserEntry {
    /// `b<n>`, unique within the herdr tab.
    pub id: String,
    #[serde(default)]
    pub kind: TabBrowserKind,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// html_view: the absolute path of the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// html_view: the page may reach the network.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_network: bool,
    /// The CDP target id while a host has the view (runtime only).
    #[serde(skip)]
    pub target: Option<String>,
    /// The host's view id (runtime only): a sync that has not learnt the
    /// minted id yet names the entry by it.
    #[serde(skip)]
    pub view_id: Option<String>,
    /// `user` or `agent:<pane_id>`.
    #[serde(default)]
    pub opened_by: String,
}

/// What one applied sync group changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncChange {
    /// Anything at all (title, target): `rev` bumped.
    pub any: bool,
    /// What persistence cares about: order, url, kind, file, network, active.
    pub persisted: bool,
}

/// A random lowercase hex string of `len` characters (not cryptographic:
/// keys and lease ids only need to be unique and unguessable in passing).
pub fn random_hex(len: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::with_capacity(len + 16);
    while out.len() < len {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        hasher.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out.truncate(len);
    out
}

/// A new group key.
pub fn new_key() -> String {
    format!("g-{}", random_hex(16))
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

impl TabBrowser {
    pub fn new(key: Option<&str>) -> Self {
        Self {
            key: key
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
                .unwrap_or_else(new_key),
            ..Self::default()
        }
    }

    /// The active entry (else the first).
    pub fn active_entry(&self) -> Option<&TabBrowserEntry> {
        self.active
            .as_deref()
            .and_then(|id| self.entries.iter().find(|entry| entry.id == id))
            .or_else(|| self.entries.first())
    }

    /// The next free `b<n>` id.
    fn next_id(&self, taken: &[String]) -> String {
        let max = self
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .chain(taken.iter().map(String::as_str))
            .filter_map(|id| id.strip_prefix('b').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        format!("b{}", max + 1)
    }

    /// Replace the list with the companion's live one (`browser.host_sync`).
    /// Unknown entry ids and new entries get minted ids, answered in
    /// `minted` as `<tab_id>/<view_id>` → id. Truncates, never refuses.
    pub fn apply_sync(
        &mut self,
        tab_id: &str,
        group: &BrowserHostSyncGroup,
        minted: &mut BTreeMap<String, String>,
    ) -> SyncChange {
        let mut entries: Vec<TabBrowserEntry> = Vec::with_capacity(group.entries.len());
        let mut new_ids: Vec<String> = Vec::new();
        let mut view_ids: Vec<(Option<String>, String)> = Vec::new();
        for wire in group.entries.iter().take(MAX_ENTRIES) {
            let view = wire.view_id.as_deref().filter(|view| !view.is_empty());
            let old = wire
                .id
                .as_deref()
                .and_then(|id| self.entries.iter().find(|entry| entry.id == id))
                .or_else(|| {
                    // no id yet: the entry this view got minted earlier
                    view.and_then(|view| {
                        self.entries
                            .iter()
                            .find(|entry| entry.view_id.as_deref() == Some(view))
                    })
                })
                .filter(|old| {
                    // an id used twice in one sync: the second is new
                    !entries.iter().any(|entry| entry.id == old.id)
                });
            let id = match old {
                Some(old) => {
                    if wire.id.as_deref() != Some(old.id.as_str()) {
                        if let Some(view) = view {
                            minted.insert(format!("{tab_id}/{view}"), old.id.clone());
                        }
                    }
                    old.id.clone()
                }
                None => {
                    let id = self.next_id(&new_ids);
                    new_ids.push(id.clone());
                    if let Some(view) = wire.view_id.as_deref().filter(|v| !v.is_empty()) {
                        minted.insert(format!("{tab_id}/{view}"), id.clone());
                    }
                    id
                }
            };
            view_ids.push((wire.view_id.clone(), id.clone()));
            entries.push(entry_from_wire(wire, id, old));
        }
        let active = group.active.as_deref().and_then(|wanted| {
            entries
                .iter()
                .find(|entry| entry.id == wanted)
                .map(|entry| entry.id.clone())
                .or_else(|| {
                    view_ids
                        .iter()
                        .find(|(view, _)| view.as_deref() == Some(wanted))
                        .map(|(_, id)| id.clone())
                })
        });
        let active = active.or_else(|| entries.first().map(|entry| entry.id.clone()));
        let persisted = active != self.active
            || entries.len() != self.entries.len()
            || entries
                .iter()
                .zip(&self.entries)
                .any(|(new, old)| persisted_differs(new, old));
        let any = persisted
            || entries
                .iter()
                .zip(&self.entries)
                .any(|(new, old)| new != old);
        if any {
            self.entries = entries;
            self.active = active;
            self.rev += 1;
        }
        SyncChange { any, persisted }
    }

    /// The group as `browser.host_state` answers it.
    pub fn wire_entries(&self) -> Vec<BrowserHostEntry> {
        self.entries
            .iter()
            .map(|entry| BrowserHostEntry {
                id: Some(entry.id.clone()),
                view_id: entry.view_id.clone(),
                target: entry.target.clone(),
                kind: entry.kind,
                url: entry.url.clone(),
                title: entry.title.clone(),
                file: entry.file.clone(),
                allow_network: entry.allow_network,
                opened_by: (!entry.opened_by.is_empty()).then(|| entry.opened_by.clone()),
            })
            .collect()
    }

    /// The snapshot form: `None` when there is nothing to restore.
    pub fn for_snapshot(&self) -> Option<TabBrowser> {
        if self.entries.is_empty() {
            return None;
        }
        let mut copy = self.clone();
        copy.rev = 0;
        for entry in &mut copy.entries {
            entry.target = None;
            entry.view_id = None;
        }
        Some(copy)
    }

    /// A restored group: targets unknown until the host syncs, ids and
    /// sizes re-checked (a hand-edited session.json).
    pub fn restored(mut self) -> Option<TabBrowser> {
        if self.key.trim().is_empty() {
            self.key = new_key();
        }
        self.entries.truncate(MAX_ENTRIES);
        let mut seen: Vec<String> = Vec::new();
        for entry in &mut self.entries {
            entry.target = None;
            entry.view_id = None;
            entry.url = clip(&entry.url, MAX_URL_CHARS);
            entry.title = clip(&entry.title, MAX_TITLE_CHARS);
            if entry.id.is_empty() || seen.contains(&entry.id) {
                entry.id = String::new();
            } else {
                seen.push(entry.id.clone());
            }
        }
        for index in 0..self.entries.len() {
            if self.entries[index].id.is_empty() {
                let id = self.next_id(&seen);
                seen.push(id.clone());
                self.entries[index].id = id;
            }
        }
        if self
            .active
            .as_deref()
            .is_some_and(|active| !self.entries.iter().any(|entry| entry.id == active))
        {
            self.active = None;
        }
        (!self.entries.is_empty()).then_some(self)
    }
}

fn persisted_differs(new: &TabBrowserEntry, old: &TabBrowserEntry) -> bool {
    new.id != old.id
        || new.kind != old.kind
        || new.url != old.url
        || new.file != old.file
        || new.allow_network != old.allow_network
}

fn entry_from_wire(
    wire: &BrowserHostEntry,
    id: String,
    old: Option<&TabBrowserEntry>,
) -> TabBrowserEntry {
    let kind = match wire.kind {
        TabBrowserKind::Unknown => old.map(|old| old.kind).unwrap_or_default(),
        kind => kind,
    };
    let file = wire
        .file
        .clone()
        .filter(|file| !file.is_empty())
        .filter(|_| kind == TabBrowserKind::HtmlView);
    TabBrowserEntry {
        id,
        kind,
        url: clip(&wire.url, MAX_URL_CHARS),
        title: clip(&wire.title, MAX_TITLE_CHARS),
        file,
        allow_network: kind == TabBrowserKind::HtmlView && wire.allow_network,
        target: wire.target.clone().filter(|target| !target.is_empty()),
        view_id: wire.view_id.clone().filter(|view| !view.is_empty()),
        opened_by: wire
            .opened_by
            .clone()
            .filter(|by| !by.is_empty())
            .or_else(|| old.map(|old| old.opened_by.clone()))
            .unwrap_or_else(|| "user".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(id: Option<&str>, view: &str, url: &str) -> BrowserHostEntry {
        BrowserHostEntry {
            id: id.map(str::to_string),
            view_id: Some(view.into()),
            target: Some(format!("T-{view}")),
            kind: TabBrowserKind::Web,
            url: url.into(),
            title: format!("title {url}"),
            ..Default::default()
        }
    }

    fn group(active: Option<&str>, entries: Vec<BrowserHostEntry>) -> BrowserHostSyncGroup {
        BrowserHostSyncGroup {
            tab_id: "w1:t1".into(),
            key: None,
            active: active.map(str::to_string),
            entries,
        }
    }

    #[test]
    fn a_sync_mints_ids_for_new_entries_and_keeps_known_ones() {
        let mut browser = TabBrowser::new(None);
        assert!(browser.key.starts_with("g-") && browser.key.len() == 18);
        let mut minted = BTreeMap::new();
        let change = browser.apply_sync(
            "w1:t1",
            &group(
                Some("v2"),
                vec![wire(None, "v1", "a"), wire(None, "v2", "b")],
            ),
            &mut minted,
        );
        assert!(change.persisted && change.any);
        assert_eq!(minted.get("w1:t1/v1").map(String::as_str), Some("b1"));
        assert_eq!(minted.get("w1:t1/v2").map(String::as_str), Some("b2"));
        assert_eq!(browser.active.as_deref(), Some("b2"), "active by view id");
        assert_eq!(browser.entries[0].target.as_deref(), Some("T-v1"));
        assert_eq!(browser.rev, 1);

        // known ids kept, a title-only change is not a persisted change
        let mut minted = BTreeMap::new();
        let mut same = group(
            Some("b2"),
            vec![wire(Some("b1"), "v1", "a"), wire(Some("b2"), "v2", "b")],
        );
        same.entries[0].title = "renamed".into();
        let change = browser.apply_sync("w1:t1", &same, &mut minted);
        assert!(minted.is_empty());
        assert!(change.any && !change.persisted);
        assert_eq!(browser.rev, 2);

        // nothing changed: no rev bump
        let change = browser.apply_sync("w1:t1", &same, &mut BTreeMap::new());
        assert_eq!(change, SyncChange::default());
        assert_eq!(browser.rev, 2);

        // a sync that has not learnt the ids yet names entries by view id
        // (contract §12.1): the same ids again, answered again
        let mut minted = BTreeMap::new();
        let unlearnt = group(
            Some("v2"),
            vec![wire(None, "v1", "a"), wire(None, "v2", "b")],
        );
        browser.apply_sync("w1:t1", &unlearnt, &mut minted);
        assert_eq!(browser.entries[0].id, "b1");
        assert_eq!(browser.entries[1].id, "b2");
        assert_eq!(browser.active.as_deref(), Some("b2"));
        assert_eq!(minted.get("w1:t1/v1").map(String::as_str), Some("b1"));
    }

    #[test]
    fn a_sync_truncates_long_fields_and_caps_the_list() {
        let mut browser = TabBrowser::new(Some("g-1"));
        let entries: Vec<_> = (0..40)
            .map(|n| {
                let mut entry = wire(None, &format!("v{n}"), &"x".repeat(3000));
                entry.title = "t".repeat(400);
                entry
            })
            .collect();
        browser.apply_sync("w1:t1", &group(None, entries), &mut BTreeMap::new());
        assert_eq!(browser.entries.len(), MAX_ENTRIES);
        assert_eq!(browser.entries[0].url.len(), MAX_URL_CHARS);
        assert_eq!(browser.entries[0].title.len(), MAX_TITLE_CHARS);
        assert_eq!(browser.active.as_deref(), Some("b1"), "first when unset");
    }

    #[test]
    fn the_snapshot_drops_targets_and_restore_repairs_ids() {
        let mut browser = TabBrowser::new(Some("g-abc"));
        browser.apply_sync(
            "w1:t1",
            &group(None, vec![wire(None, "v1", "a"), wire(None, "v2", "b")]),
            &mut BTreeMap::new(),
        );
        let snap = browser.for_snapshot().expect("entries");
        assert!(snap.entries.iter().all(|entry| entry.target.is_none()));
        let json = serde_json::to_value(&snap).unwrap();
        assert!(json.get("rev").is_none());
        assert!(json["entries"][0].get("target").is_none());
        let mut back: TabBrowser = serde_json::from_value(json).unwrap();
        back.entries[1].id = "b1".into(); // a duplicate after a hand edit
        let back = back.restored().expect("entries");
        assert_eq!(back.entries[0].id, "b1");
        assert_eq!(back.entries[1].id, "b2");
        assert!(TabBrowser::new(None).for_snapshot().is_none());
    }
}
