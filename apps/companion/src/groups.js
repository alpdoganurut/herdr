// Group bookkeeping (contract §2/§3): herdr tab -> group {key, entries[], active}.
// Pure data: no Electron. views.js attaches live WebContentsViews to entries by view_id.
'use strict';

const MAX_ENTRIES = 32;
const MAX_URL = 2048;
const MAX_TITLE = 300;

const clip = (s, n) => (typeof s === 'string' ? (s.length > n ? s.slice(0, n) : s) : '');

class GroupStore {
  constructor() {
    this.groups = new Map(); // tab_id -> group
    this.tabs = new Map(); // tab_id -> {tab_id, workspace_id, label}  (every herdr tab we know)
    this.focusedTabId = null;
    this.nextView = 1;
    this.dirty = new Set(); // tab_ids whose group changed since the last sync
    this.seq = 0;
  }

  mintViewId() {
    return 'v' + this.nextView++;
  }

  group(tabId, { create = false } = {}) {
    let g = this.groups.get(tabId);
    if (!g && create) {
      g = { tab_id: tabId, key: null, entries: [], active: null };
      this.groups.set(tabId, g);
    }
    return g || null;
  }

  groupByKey(key) {
    if (!key) return null;
    for (const g of this.groups.values()) if (g.key === key) return g;
    return null;
  }

  findView(viewId) {
    for (const g of this.groups.values()) {
      const e = g.entries.find((x) => x.view_id === viewId);
      if (e) return { group: g, entry: e };
    }
    return null;
  }

  label(tabId) {
    const t = this.tabs.get(tabId);
    return t ? t.label : tabId;
  }

  markDirty(tabId) {
    this.dirty.add(tabId);
  }

  // Adds an entry (a live view or a placeholder) and returns it.
  addEntry(tabId, entry, { background = true, index } = {}) {
    const g = this.group(tabId, { create: true });
    if (g.entries.length >= MAX_ENTRIES) {
      const err = new Error(`a session holds at most ${MAX_ENTRIES} pages`);
      err.code = 'too_many_views';
      throw err;
    }
    const e = {
      id: entry.id || null,
      view_id: entry.view_id || this.mintViewId(),
      target: entry.target || null,
      kind: entry.kind || 'web',
      url: clip(entry.url || 'about:blank', MAX_URL),
      title: clip(entry.title || '', MAX_TITLE),
      file: entry.file || null,
      root: entry.root || null,
      allow_network: !!entry.allow_network,
      // page API: an agent's html view without network only (main also checks the lease feature)
      page_api: entry.kind === 'html_view' && !entry.allow_network && !!entry.page_api,
      opener: entry.opener || null,
      opened_by: entry.opened_by || 'user',
      title_fixed: !!entry.title_fixed, // runtime only: a restored view loses it
    };
    if (index === undefined || index < 0 || index > g.entries.length) g.entries.push(e);
    else g.entries.splice(index, 0, e);
    if (!background || !g.active) g.active = e.view_id;
    this.markDirty(tabId);
    return e;
  }

  removeEntry(viewId) {
    const hit = this.findView(viewId);
    if (!hit) return null;
    const { group: g, entry } = hit;
    const i = g.entries.indexOf(entry);
    g.entries.splice(i, 1);
    if (g.active === viewId) {
      const next = g.entries[i] || g.entries[i - 1] || null;
      g.active = next ? next.view_id : null;
    }
    this.markDirty(g.tab_id);
    return { group: g, entry };
  }

  select(viewId) {
    const hit = this.findView(viewId);
    if (!hit) return null;
    if (hit.group.active !== viewId) {
      hit.group.active = viewId;
      this.markDirty(hit.group.tab_id);
    }
    return hit;
  }

  // Updates url/title/target of an entry; marks dirty only on a real change.
  updateEntry(viewId, patch) {
    const hit = this.findView(viewId);
    if (!hit) return null;
    const e = hit.entry;
    let changed = false;
    for (const [k, v0] of Object.entries(patch)) {
      if (k === 'title' && e.title_fixed) continue;
      const v = k === 'url' ? clip(v0, MAX_URL) : k === 'title' ? clip(v0, MAX_TITLE) : v0;
      if (e[k] !== v) {
        e[k] = v;
        changed = true;
      }
    }
    if (changed) this.markDirty(hit.group.tab_id);
    return hit;
  }

  // Merges browser.host_state (startup / reconnect / safety-net re-read).
  // Live groups win (the companion owns the views); herdr's state seeds groups we do not have.
  // Returns {created:[tab_id], dropped:[group], moved:[{from,to}]}.
  applyHostState(state, { dropMissing = true } = {}) {
    const out = { created: [], dropped: [], moved: [] };
    const seen = new Set();
    this.tabs = new Map();
    for (const t of state.tabs || []) {
      this.tabs.set(t.tab_id, { tab_id: t.tab_id, workspace_id: t.workspace_id, label: t.label || t.tab_id });
    }
    for (const t of state.tabs || []) {
      let g = (t.key && this.groupByKey(t.key)) || null;
      if (g && g.tab_id !== t.tab_id) {
        // same key under a new tab id (cross-workspace move)
        const old = g.tab_id;
        this.groups.delete(old);
        g.tab_id = t.tab_id;
        this.groups.set(t.tab_id, g);
        out.moved.push({ from: old, to: t.tab_id });
        this.markDirty(t.tab_id);
      }
      if (!g) {
        // A live group under this tab id always wins over herdr's stored entries (its views
        // are real); if herdr knows the tab under another key, adopt herdr's key and resync.
        g = this.groups.get(t.tab_id) || null;
      }
      if (g) {
        if (t.key && g.key !== t.key) {
          g.key = t.key;
          this.markDirty(g.tab_id);
        }
        seen.add(g);
        continue;
      }
      if (t.key && Array.isArray(t.entries) && t.entries.length) {
        // restore: placeholders, views are created lazily when the group is shown
        g = { tab_id: t.tab_id, key: t.key, entries: [], active: null };
        this.groups.set(t.tab_id, g);
        for (const e of t.entries.slice(0, MAX_ENTRIES)) {
          const entry = {
            id: e.id,
            view_id: this.mintViewId(),
            target: null,
            kind: e.kind === 'html_view' ? 'html_view' : 'web',
            url: clip(e.url || 'about:blank', MAX_URL),
            title: clip(e.title || '', MAX_TITLE),
            file: e.file || null,
            root: e.file ? require('node:path').dirname(e.file) : null,
            allow_network: !!e.allow_network,
            page_api: e.kind === 'html_view' && !e.allow_network && !!e.page_api,
            opener: e.opener || null,
            opened_by: e.opened_by || 'user',
            placeholder: true,
          };
          g.entries.push(entry);
          if (e.id === t.active) g.active = entry.view_id;
        }
        if (!g.active && g.entries.length) g.active = g.entries[0].view_id;
        seen.add(g);
        out.created.push(t.tab_id);
      }
    }
    if (dropMissing) {
      for (const g of [...this.groups.values()]) {
        if (!seen.has(g) && !this.tabs.has(g.tab_id)) {
          this.groups.delete(g.tab_id);
          this.dirty.delete(g.tab_id);
          out.dropped.push(g);
        }
      }
    }
    if (state.focused_tab_id !== undefined) this.focusedTabId = state.focused_tab_id;
    return out;
  }

  dropGroup(tabId) {
    const g = this.groups.get(tabId);
    if (!g) return null;
    this.groups.delete(tabId);
    this.dirty.delete(tabId);
    return g;
  }

  // Builds the next browser.host_sync payload for the dirty groups, or null.
  // Placeholders are synced too (full replace per group). With `maxBytes`, whole groups are
  // taken until the payload's groups would pass it (always at least one; one over it alone
  // gets its titles cut); the rest stay dirty for the next sync, which gets its own seq.
  takeSync(leaseId, { all = false, maxBytes = Infinity } = {}) {
    const ids = all ? [...this.groups.keys()] : [...this.dirty].filter((id) => this.groups.has(id));
    for (const id of [...this.dirty]) if (!this.groups.has(id)) this.dirty.delete(id);
    if (!ids.length) return null;
    const groups = [];
    let bytes = 2; // the array's brackets
    for (const id of ids) {
      const out = this.syncGroup(this.groups.get(id));
      let size = Buffer.byteLength(JSON.stringify(out)) + 1;
      if (!groups.length && size > maxBytes) {
        for (const x of out.entries) x.title = x.title.slice(0, 80);
        size = Buffer.byteLength(JSON.stringify(out)) + 1;
      }
      if (groups.length && bytes + size > maxBytes) break;
      groups.push(out);
      bytes += size;
      this.dirty.delete(id);
    }
    if (all) for (const id of ids.slice(groups.length)) this.dirty.add(id);
    this.seq += 1;
    return { lease_id: leaseId, seq: this.seq, groups };
  }

  syncGroup(g) {
    const active = g.entries.find((e) => e.view_id === g.active);
    const out = {
      tab_id: g.tab_id,
      active: active ? active.id || active.view_id : null,
      entries: g.entries.map((e) => {
        const x = { view_id: e.view_id, kind: e.kind, url: e.url, title: e.title };
        if (e.id) x.id = e.id;
        if (e.target) x.target = e.target;
        if (e.kind === 'html_view') {
          x.file = e.file;
          x.allow_network = !!e.allow_network;
          if (e.page_api) x.page_api = true;
          if (e.page_api && e.opener) x.opener = e.opener;
        }
        if (e.opened_by) x.opened_by = e.opened_by;
        return x;
      }),
    };
    if (g.key) out.key = g.key;
    return out;
  }

  // Applies browser.host_synced: minted keys and entry ids. Returns the tab ids herdr did
  // not know: never dropped here (a cross-workspace move renames the tab, and the views are
  // live); the caller re-reads host_state, which follows the group's key or drops it.
  applySynced(result) {
    for (const [k, v] of Object.entries(result.keys || {})) {
      const slash = k.lastIndexOf('/'); // "<tab_id>/<view_id>": view ids never hold a slash
      if (slash < 0) {
        const g = this.groups.get(k);
        if (g) g.key = v;
      } else {
        const tabId = k.slice(0, slash);
        const viewId = k.slice(slash + 1);
        const g = this.groups.get(tabId);
        const e = g && g.entries.find((x) => x.view_id === viewId);
        if (e) e.id = v;
      }
    }
    return (result.unknown_tabs || []).filter((tabId) => this.groups.has(tabId));
  }

  // A new lease restarts seq and requires a full resync.
  resetLease() {
    this.seq = 0;
    for (const id of this.groups.keys()) this.dirty.add(id);
  }
}

module.exports = { GroupStore, MAX_ENTRIES, MAX_URL, MAX_TITLE };
