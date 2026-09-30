// herdr companion: tab groups for agent panes. Driven over CDP by herdr's
// sidecar (Runtime.evaluate on this worker); nothing here runs on its own.
// The listeners and the alarm keep the worker wakeable after Chrome idles it
// out; the sidecar's held DevTools connection keeps it alive in between.
for (const ev of [chrome.tabs.onCreated, chrome.tabs.onUpdated, chrome.tabs.onRemoved, chrome.tabs.onActivated]) ev.addListener(() => {});
chrome.tabGroups.onUpdated.addListener(() => {});
chrome.alarms.onAlarm.addListener(() => {});
chrome.alarms.create('herdr-keepalive', { periodInMinutes: 0.5 });

// Bumped with every change to this file; the sidecar reports a running
// worker that still answers an older number (COMPANION_VERSION in activity.mjs);
// the next browser launch loads the new files.
const VERSION = 2;
self.herdrPing = () => 'herdr-companion/' + VERSION;

self.herdrTabs = async () => (await chrome.tabs.query({})).map((t) => ({
  id: t.id, url: t.url || t.pendingUrl || '', title: t.title || '', windowId: t.windowId, groupId: t.groupId, active: t.active,
}));

// Put a tab into the pane's group (creating or re-finding it), expanded.
// A tab the user pulled out of our group (wasGrouped, now ungrouped) or put
// into a group of their own is left alone.
self.herdrGroup = async ({ tabId, groupId, title, color, wasGrouped }) => {
  const tab = await chrome.tabs.get(tabId);
  if (wasGrouped && tab.groupId === -1) return { skipped: 'user_ungrouped' };
  if (tab.groupId !== -1 && tab.groupId !== groupId) {
    let info = null;
    try { info = await chrome.tabGroups.get(tab.groupId); } catch {}
    if (!info || info.title !== title) return { skipped: 'other_group', groupId: tab.groupId };
    groupId = tab.groupId;
  }
  let g = groupId;
  if (g != null) {
    try { if ((await chrome.tabGroups.get(g)).windowId !== tab.windowId) g = null; } catch { g = null; }
  }
  if (g == null) {
    const found = await chrome.tabGroups.query({ title, windowId: tab.windowId }).catch(() => []);
    if (found.length) g = found[0].id;
  }
  if (tab.groupId !== g) g = await chrome.tabs.group(g != null ? { tabIds: [tabId], groupId: g } : { tabIds: [tabId] });
  await chrome.tabGroups.update(g, { title, color, collapsed: false });
  return { groupId: g };
};

self.herdrCollapse = async (groupId) => {
  try { await chrome.tabGroups.update(groupId, { collapsed: true }); return { collapsed: true }; }
  catch (err) { return { collapsed: false, error: String(err && err.message || err) }; }
};

// Every group carrying this title (a pane that is gone), in any window.
self.herdrDissolve = async (title) => {
  let count = 0;
  for (const group of await chrome.tabGroups.query({ title }).catch(() => [])) {
    const tabs = await chrome.tabs.query({ groupId: group.id }).catch(() => []);
    if (tabs.length) { try { await chrome.tabs.ungroup(tabs.map((t) => t.id)); count += tabs.length; } catch {} }
  }
  return { ungrouped: count };
};

self.herdrUngroup = async (tabIds) => {
  const ungrouped = [];
  for (const id of tabIds) { try { await chrome.tabs.ungroup(id); ungrouped.push(id); } catch {} }
  return { ungrouped };
};
