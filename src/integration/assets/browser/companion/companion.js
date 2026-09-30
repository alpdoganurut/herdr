// herdr companion worker code (imported by sw.js under the manifest version).
for (const ev of [chrome.tabs.onCreated, chrome.tabs.onUpdated, chrome.tabs.onRemoved, chrome.tabs.onActivated]) ev.addListener(() => {});
chrome.tabGroups.onUpdated.addListener(() => {});
chrome.alarms.onAlarm.addListener(() => {});
chrome.alarms.create('herdr-keepalive', { periodInMinutes: 0.5 });

// Bumped with every change to this file, together with the manifest's
// version (0.<VERSION>.0, which is what makes sw.js import a fresh copy) and
// COMPANION_VERSION in activity.mjs; the sidecar reports a running worker
// that still answers an older number.
const VERSION = 3;
self.herdrPing = () => 'herdr-companion/' + VERSION;

self.herdrTabs = async () => (await chrome.tabs.query({})).map((t) => ({
  id: t.id, url: t.url || t.pendingUrl || '', title: t.title || '', windowId: t.windowId, groupId: t.groupId, active: t.active,
}));

// Groups are the panes': key (pane id) → group id, in session storage (it
// survives a sidecar restart, not a browser restart). A group is only ever
// found by its recorded id; a same-named group of the user's is never touched.
const store = {
  async get(key) { const r = await chrome.storage.session.get('groups'); return (r.groups || {})[key]; },
  async set(key, id) {
    const r = await chrome.storage.session.get('groups');
    const groups = r.groups || {};
    if (id == null) delete groups[key]; else groups[key] = id;
    await chrome.storage.session.set({ groups });
  },
};

// Put a tab into the pane's group (creating it when the pane has none),
// expanded. A tab the user pulled out of our group (wasGrouped, now
// ungrouped) or that sits in any other group is left alone.
self.herdrGroup = async ({ key, tabId, title, color, wasGrouped }) => {
  const tab = await chrome.tabs.get(tabId);
  if (wasGrouped && tab.groupId === -1) return { skipped: 'user_ungrouped' };
  let g = await store.get(key);
  if (g != null) {
    try { if ((await chrome.tabGroups.get(g)).windowId !== tab.windowId) g = null; } catch { g = null; }
  }
  if (tab.groupId !== -1 && tab.groupId !== g) return { skipped: 'other_group', groupId: tab.groupId };
  if (g == null) g = await chrome.tabs.group({ tabIds: [tabId] });
  else if (tab.groupId !== g) await chrome.tabs.group({ tabIds: [tabId], groupId: g });
  await store.set(key, g);
  await chrome.tabGroups.update(g, { title, color, collapsed: false });
  return { groupId: g };
};

self.herdrCollapse = async (key) => {
  const g = await store.get(key);
  if (g == null) return { collapsed: false };
  try { await chrome.tabGroups.update(g, { collapsed: true }); return { collapsed: true }; }
  catch (err) { return { collapsed: false, error: String(err && err.message || err) }; }
};

// The pane is gone: its recorded group dissolves (every tab in it ungrouped).
self.herdrRelease = async (key) => {
  const g = await store.get(key);
  await store.set(key, null);
  if (g == null) return { ungrouped: 0 };
  let tabs = [];
  try { tabs = await chrome.tabs.query({ groupId: g }); } catch {}
  if (tabs.length) await chrome.tabs.ungroup(tabs.map((t) => t.id));
  return { ungrouped: tabs.length };
};
