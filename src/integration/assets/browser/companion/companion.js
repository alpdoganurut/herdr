// herdr companion worker code (imported by sw.js under the manifest version).
for (const ev of [chrome.tabs.onCreated, chrome.tabs.onUpdated, chrome.tabs.onRemoved, chrome.tabs.onActivated]) ev.addListener(() => {});
chrome.tabGroups.onUpdated.addListener(() => {});
chrome.alarms.onAlarm.addListener(() => {});
chrome.alarms.create('herdr-keepalive', { periodInMinutes: 0.5 });

// Bumped with every change to this file, together with the manifest's
// version (0.<VERSION>.0), COMPANION_VERSION in activity.mjs and
// COMPANION_VERSION in browser_assets.rs (herdr clears a profile's worker
// store before launching it with a new version, so the new code loads).
const VERSION = 7;
self.herdrPing = () => 'herdr-companion/' + VERSION;

// The new tab page's data: herdr's compact snapshot, kept in session storage
// (the page renders it and follows storage.onChanged).
self.herdrSnapshot = async (snapshot) => {
  await chrome.storage.session.set({ snapshot });
  return { ok: true };
};

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

// ---------------------------------------------------------------------------
// The pinned dashboard: the herdr+ page as a pinned first tab in every normal
// window (`[browser] pin_dashboard`, told by herdr at attach and on config
// changes, remembered in storage.local for the next browser start). A window
// whose user unpinned or closed it is left alone for the session
// (storage.session); older herdr pages (another extension id) are closed.
const DASHBOARD_URL = chrome.runtime.getURL('dashboard.html');
const isOurDashboard = (t) => t.url === DASHBOARD_URL || t.pendingUrl === DASHBOARD_URL;
const isStaleHerdrPage = (t) => {
  const url = t.url || t.pendingUrl || '';
  return /^chrome-extension:\/\/[a-p]{32}\/(dashboard|newtab)\.html$/.test(url) && !url.startsWith(chrome.runtime.getURL(''));
};
async function dashboardPinned() {
  const { dashboardPin } = await chrome.storage.local.get('dashboardPin');
  return dashboardPin !== false;
}
async function dashboardSession() {
  const s = await chrome.storage.session.get(['dashboardDismissed', 'dashboardTabs']);
  return { dismissed: s.dashboardDismissed || {}, tabs: s.dashboardTabs || {} };
}
async function ensureDashboardWindow(win, pin, session) {
  if (win.type && win.type !== 'normal') return;
  const tabs = await chrome.tabs.query({ windowId: win.id });
  const stale = tabs.filter(isStaleHerdrPage);
  if (stale.length) await chrome.tabs.remove(stale.map((t) => t.id)).catch(() => {});
  const ours = tabs.filter(isOurDashboard).sort((a, b) => a.index - b.index);
  if (!pin) {
    if (ours.length) await chrome.tabs.remove(ours.map((t) => t.id)).catch(() => {});
    return;
  }
  if (ours.length > 1) {
    for (const extra of ours.slice(1)) delete session.tabs[extra.id];
    await chrome.storage.session.set({ dashboardTabs: session.tabs });
    await chrome.tabs.remove(ours.slice(1).map((t) => t.id)).catch(() => {});
  }
  const keep = ours[0];
  if (keep) {
    if (!keep.pinned) { session.dismissed[win.id] = true; return; }
    if (keep.index !== 0) await chrome.tabs.move(keep.id, { index: 0 }).catch(() => {});
    session.tabs[keep.id] = win.id;
    return;
  }
  if (session.dismissed[win.id]) return;
  const made = await chrome.tabs.create({ windowId: win.id, url: DASHBOARD_URL, pinned: true, index: 0, active: false });
  session.tabs[made.id] = win.id;
}
// While herdr itself adds or removes dashboard tabs the listeners stay quiet:
// Chrome reports a removed pinned tab as an unpin first.
async function suppressed() {
  const { dashboardBusyUntil } = await chrome.storage.session.get('dashboardBusyUntil');
  return typeof dashboardBusyUntil === 'number' && Date.now() < dashboardBusyUntil;
}
async function ensureDashboards(pin) {
  const session = pin ? await dashboardSession() : { dismissed: {}, tabs: {} };
  await chrome.storage.session.set({ dashboardBusyUntil: Date.now() + 3000 });
  // Forget the tracked tabs before removing any: a removal herdr does must not
  // read as the user closing it.
  if (!pin) await chrome.storage.session.set({ dashboardDismissed: {}, dashboardTabs: {} });
  const wins = await chrome.windows.getAll({ windowTypes: ['normal'] });
  for (const win of wins) await ensureDashboardWindow(win, pin, session).catch(() => {});
  await chrome.storage.session.set({ dashboardDismissed: session.dismissed, dashboardTabs: session.tabs, dashboardBusyUntil: Date.now() + 1000 });
  return { windows: wins.length, pin };
}
self.herdrDashboard = async ({ pin }) => {
  await chrome.storage.local.set({ dashboardPin: Boolean(pin) });
  return ensureDashboards(Boolean(pin));
};
chrome.windows.onCreated.addListener(async (win) => {
  if (!(await dashboardPinned())) return;
  setTimeout(async () => {
    const session = await dashboardSession();
    await ensureDashboardWindow(win, true, session).catch(() => {});
    await chrome.storage.session.set({ dashboardDismissed: session.dismissed, dashboardTabs: session.tabs });
  }, 400);
});
chrome.tabs.onRemoved.addListener(async (tabId, info) => {
  if (info.isWindowClosing || await suppressed()) return;
  const session = await dashboardSession();
  const windowId = session.tabs[tabId];
  if (windowId === undefined) return;
  delete session.tabs[tabId];
  session.dismissed[windowId] = true; // the user closed it: not again this session
  await chrome.storage.session.set({ dashboardDismissed: session.dismissed, dashboardTabs: session.tabs });
});
chrome.tabs.onUpdated.addListener(async (tabId, change, tab) => {
  if (change.pinned !== false || !isOurDashboard(tab) || await suppressed()) return;
  // A closing pinned tab reports an unpin first: only a tab still there, still unpinned, counts.
  setTimeout(async () => {
    try {
      const still = await chrome.tabs.get(tabId);
      if (still.pinned || !isOurDashboard(still)) return;
      const session = await dashboardSession();
      session.dismissed[still.windowId] = true; // unpinned by the user
      await chrome.storage.session.set({ dashboardDismissed: session.dismissed });
    } catch {}
  }, 600);
});
// At every worker start (browser launch included): what the last attach said, default on.
dashboardPinned().then((pin) => ensureDashboards(pin)).catch(() => {});

