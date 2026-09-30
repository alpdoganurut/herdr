// The activity overlay: the glow frame and the cursor injected into a page
// (an isolated world, a closed shadow root, no pointer events) while an agent
// works on it, and the companion extension's per-pane tab groups, driven over
// CDP. The look is the approved one from the overlay spike; its values are
// reused verbatim.

export const WORLD = 'herdr';
export const HOST_ATTR = 'data-herdr-overlay';
/** The companion worker code this sidecar expects (VERSION in companion/sw.js); an older running worker is reloaded. */
export const COMPANION_VERSION = 2;
/** The frame stays this long after the last operation. */
export const LINGER_MS = 3000;
/** The cursor's glide (matches the CSS transition). */
export const GLIDE_MS = 350;
/** One overlay evaluate may take at most this long; the op never waits longer. */
const EVAL_TIMEOUT_MS = 1500;
const COMPANION_CALL_MS = 3000;
/** The blank tab that wakes an idled companion worker; never tracked. */
export const NUDGE_URL = 'about:blank#herdr-nudge';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function withTimeout(promise, ms, what) {
  let timer;
  const timeout = new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${what} took more than ${ms} ms`)), ms); });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/** The approved purple; `[browser] activity_color` overrides it per directive. */
export const DEFAULT_COLOR = '#aa6eff';
export function normalizeColor(value) {
  const text = String(value || '').trim().toLowerCase();
  return /^#[0-9a-f]{6}$/.test(text) ? text : DEFAULT_COLOR;
}

// Installed in the isolated world: `(OVERLAY_JS)(cursor, color)`; idempotent per
// document, re-installed when the colour changed. The alpha values are the
// approved look; only the rgb comes from the colour.
export const OVERLAY_JS = `((cursor, color) => {
  const rgb = [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16)).join(',');
  const live = window.__herdrOverlay;
  if (live && live.alive && live.color === color) { live.show(); return 'present'; }
  if (live && live.alive) { cursor = live.cursor || cursor; live.alive = false; }
  document.querySelectorAll('div[${HOST_ATTR}]').forEach((n) => n.remove());
  const host = document.createElement('div');
  host.setAttribute('${HOST_ATTR}', '');
  host.setAttribute('data-herdr-color', color);
  host.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;opacity:0;transition:opacity .25s';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = \`<style>
    .frame{position:fixed;inset:0;border:2px solid rgba(\${rgb},.9);box-shadow:inset 0 0 14px 3px rgba(\${rgb},.42);
      border-radius:6px;animation:pulse 2s ease-in-out infinite}
    @keyframes pulse{50%{box-shadow:inset 0 0 22px 6px rgba(\${rgb},.62)}}
    .cur{position:fixed;left:-5px;top:-2.5px;width:24px;height:24px;transition:transform .35s cubic-bezier(.2,.8,.2,1);
      filter:drop-shadow(0 1px 2px rgba(0,0,0,.5))}
    .ripple{position:fixed;width:28px;height:28px;margin:-14px 0 0 -14px;border-radius:50%;border:2px solid rgba(\${rgb},.9);
      animation:rip .5s ease-out forwards}
    @keyframes rip{from{transform:scale(.3);opacity:1}to{transform:scale(1.6);opacity:0}}
  </style><div class="frame"></div>
  <svg class="cur" viewBox="0 0 24 24"><path d="M5 2.5v17.2l4.3-4.2 2.9 6.6 2.6-1.1-2.9-6.5h6.1z" fill="\${color}" stroke="#fff" stroke-width="1.4" stroke-linejoin="round"/></svg>\`;
  (document.documentElement || document).appendChild(host);
  const cur = root.querySelector('.cur');
  if (cursor) {
    cur.style.transition = 'none';
    cur.style.transform = 'translate(' + cursor.x + 'px,' + cursor.y + 'px)';
    void cur.offsetWidth;
    cur.style.transition = '';
  } else {
    cur.style.opacity = '0';
  }
  requestAnimationFrame(() => { host.style.opacity = '1'; });
  let hideTimer = null;
  const api = {
    alive: true,
    color,
    cursor: cursor || null,
    show() { if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; } host.style.opacity = '1'; },
    hide() {
      host.style.opacity = '0';
      hideTimer = setTimeout(() => { api.alive = false; host.remove(); if (window.__herdrOverlay === api) delete window.__herdrOverlay; }, 300);
    },
    suspend(on) { host.style.visibility = on ? 'hidden' : ''; },
    moveTo(x, y, animate) {
      api.cursor = { x, y };
      cur.style.opacity = '1';
      if (!animate) cur.style.transition = 'none';
      cur.style.transform = 'translate(' + x + 'px,' + y + 'px)';
      if (!animate) { void cur.offsetWidth; cur.style.transition = ''; }
    },
    click(x, y) {
      const r = document.createElement('div');
      r.className = 'ripple'; r.style.left = x + 'px'; r.style.top = y + 'px';
      root.appendChild(r); setTimeout(() => r.remove(), 600);
    },
  };
  window.__herdrOverlay = api;
  return 'installed';
})`;

/** The overlay of one page: an isolated-world context per document, the cursor's last position, the linger timer. */
export class Overlay {
  constructor(state) {
    this.state = state;
    this.ctx = null;
    this.cursor = null;
    this.color = DEFAULT_COLOR;
    this.hideTimer = null;
    this.until = 0;
    this.log = () => {};
  }
  async context() {
    if (this.ctx != null) return this.ctx;
    const session = this.state.session;
    const tree = await session.send('Page.getFrameTree');
    const frameId = tree.frameTree.frame.id;
    const { executionContextId } = await session.send('Page.createIsolatedWorld', { frameId, worldName: WORLD, grantUniveralAccess: false });
    this.ctx = executionContextId;
    return this.ctx;
  }
  async eval(expression) {
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const contextId = await this.context();
        const r = await withTimeout(this.state.session.send('Runtime.evaluate', { expression, contextId, returnByValue: true }), EVAL_TIMEOUT_MS, 'overlay');
        if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception && r.exceptionDetails.exception.description || r.exceptionDetails.text || 'overlay script failed');
        return r.result ? r.result.value : undefined;
      } catch (err) {
        this.ctx = null;
        if (attempt === 1 || !/context|Cannot find|detached|destroyed|navigat/i.test(String(err.message))) throw err;
      }
    }
    return undefined;
  }
  /** The frame is up (installed if needed, re-installed on a colour change) and stays until `linger`. */
  async show(color) {
    if (color) this.color = normalizeColor(color);
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    this.until = Infinity;
    return this.eval(`(${OVERLAY_JS})(${JSON.stringify(this.cursor)}, ${JSON.stringify(this.color)})`);
  }
  /** The op is done: fade out after `ms`. */
  linger(ms = LINGER_MS) {
    this.until = Date.now() + ms;
    if (this.hideTimer) clearTimeout(this.hideTimer);
    this.hideTimer = setTimeout(() => {
      this.hideTimer = null;
      this.until = 0;
      this.eval('window.__herdrOverlay ? (__herdrOverlay.hide(), "hidden") : "absent"').catch(() => {});
    }, ms);
    if (this.hideTimer.unref) this.hideTimer.unref();
  }
  /** Glide the cursor to a viewport point (and wait for the glide when animating). */
  async moveTo(x, y, animate) {
    this.cursor = { x, y };
    await this.eval(`__herdrOverlay.moveTo(${x},${y},${animate ? 'true' : 'false'})`);
    if (animate) await sleep(GLIDE_MS);
  }
  ripple(x, y) {
    return this.eval(`__herdrOverlay.click(${x},${y})`).catch(() => {});
  }
  /** Hidden while a screenshot is taken (the agent's picture is the page, not the overlay). */
  suspend(on) {
    if (this.until <= Date.now()) return Promise.resolve();
    return this.eval(`window.__herdrOverlay && __herdrOverlay.suspend(${on ? 'true' : 'false'})`).catch(() => {});
  }
  /** The main frame navigated: the world is gone with the document. */
  navigated() { this.ctx = null; }
  /** A new document while the frame should be up: put it back. */
  async reapply() {
    if (this.until > Date.now()) await this.show().catch((err) => this.log(`overlay reapply: ${err.message}`));
  }
  dispose() {
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    this.until = 0;
  }
}

/** The companion extension of one profile: the worker connection, target → tab ids, the pane groups. */
export class Companion {
  constructor(profile) {
    this.profile = profile;
    this.ws = null;
    this.pending = new Map();
    this.id = 0;
    this.state = 'unknown';
    this.detail = '';
    this.tabIds = new Map(); // targetId -> extension tab id
    this.groups = new Map(); // pane key -> { groupId, tabs: Set<tabId>, timer }
    this.userUngrouped = new Set();
    this.chain = Promise.resolve();
    this.log = () => {};
  }
  info() { return { state: this.state, detail: this.detail }; }
  async targets() {
    const res = await fetch(`http://127.0.0.1:${this.profile.port}/json/list`);
    return res.json();
  }
  findIn(list) { return list.find((t) => t.type === 'service_worker' && /\/sw\.js$/.test(t.url)) || null; }
  /** Is the extension loaded, and current? (at attach) A worker still running
   *  older code (the files were refreshed under it) is reported, not reloaded:
   *  chrome.runtime.reload() on a command-line extension whose permissions
   *  grew disables it until the user re-enables it; a browser restart loads
   *  the new files cleanly. */
  async probe() {
    if (typeof WebSocket !== 'function') { this.state = 'unsupported'; this.detail = 'node has no WebSocket'; return this.info(); }
    try {
      const sw = await this.worker(true);
      if (!sw) { this.state = 'missing'; this.detail = 'no companion service worker on the DevTools port'; return this.info(); }
      this.state = 'ready';
      this.detail = '';
      const ping = String(await this.call('herdrPing').catch(() => ''));
      const version = Number((ping.split('/')[1] || '0'));
      if (version !== COMPANION_VERSION) {
        this.detail = `worker v${version || '?'}, this herdr expects v${COMPANION_VERSION}; \`herdr browser stop\` and open again to update tab groups`;
      }
    } catch (err) {
      this.state = 'missing';
      this.detail = String(err.message).slice(0, 120);
    }
    return this.info();
  }
  /** The worker target, woken by a tab event when Chrome idled it out. */
  async worker(wake) {
    let sw = this.findIn(await this.targets());
    if (!sw && wake) {
      await this.nudge();
      for (let i = 0; i < 15 && !sw; i++) { await sleep(100); sw = this.findIn(await this.targets()); }
    }
    return sw;
  }
  async nudge() {
    const browser = this.profile.browser;
    if (!browser) return;
    let session = null;
    try {
      session = await browser.newBrowserCDPSession();
      const { targetId } = await session.send('Target.createTarget', { url: NUDGE_URL, background: true });
      await sleep(50);
      await session.send('Target.closeTarget', { targetId });
    } catch (err) {
      this.log(`companion nudge: ${err.message}`);
    } finally {
      if (session) await session.detach().catch(() => {});
    }
  }
  async connect() {
    if (this.ws && this.ws.readyState === 1) return this.ws;
    const sw = await this.worker(true);
    if (!sw) { this.state = 'missing'; throw new Error('companion: no service worker'); }
    const ws = new WebSocket(sw.webSocketDebuggerUrl);
    await withTimeout(new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('companion: websocket failed')); }), COMPANION_CALL_MS, 'companion connect');
    ws.onmessage = (m) => {
      let msg; try { msg = JSON.parse(m.data); } catch { return; }
      const waiter = this.pending.get(msg.id);
      if (waiter) { this.pending.delete(msg.id); waiter(msg); }
    };
    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      for (const waiter of this.pending.values()) waiter({ error: { message: 'companion connection closed' } });
      this.pending.clear();
    };
    ws.onerror = () => {};
    this.ws = ws;
    this.state = 'ready';
    return ws;
  }
  async call(fn, arg) {
    const ws = await this.connect();
    const id = ++this.id;
    const expression = `${fn}(${JSON.stringify(arg === undefined ? null : arg)})`;
    const reply = await withTimeout(new Promise((resolve) => {
      this.pending.set(id, resolve);
      ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
    }), COMPANION_CALL_MS, `companion ${fn}`).finally(() => this.pending.delete(id));
    if (reply.error) throw new Error(reply.error.message || 'companion call failed');
    const details = reply.result && reply.result.exceptionDetails;
    if (details) throw new Error(details.exception && details.exception.description || details.text || 'companion call failed');
    return reply.result && reply.result.result ? reply.result.result.value : undefined;
  }
  /** Right after `Target.createTarget('about:blank')`: the newest blank tab not yet known is this target's. */
  async adoptNew(targetId) {
    const tabs = await this.call('herdrTabs');
    const known = new Set(this.tabIds.values());
    const candidates = tabs.filter((t) => t.url === 'about:blank' && !known.has(t.id)).sort((a, b) => b.id - a.id);
    if (candidates.length) this.tabIds.set(targetId, candidates[0].id);
    else this.log(`companion: no blank tab to adopt for ${targetId}`);
  }
  /** The extension tab id of a tracked page: cached, else the one tab with its URL (and title). */
  async tabIdFor(state) {
    const cached = this.tabIds.get(state.target);
    if (cached != null) return cached;
    const url = state.page.url();
    const tabs = await this.call('herdrTabs');
    const known = new Set(this.tabIds.values());
    let candidates = tabs.filter((t) => t.url === url && !known.has(t.id));
    if (candidates.length > 1) {
      const title = await state.page.title().catch(() => '');
      candidates = candidates.filter((t) => t.title === title);
    }
    if (candidates.length !== 1) { this.log(`companion: ${candidates.length} tabs match ${url}; not grouped`); return null; }
    this.tabIds.set(state.target, candidates[0].id);
    return candidates[0].id;
  }
  forget(targetId) {
    const tabId = this.tabIds.get(targetId);
    this.tabIds.delete(targetId);
    if (tabId != null) for (const g of this.groups.values()) g.tabs.delete(tabId);
  }
  /** An agent pane touched a tab: it sits in the pane's group, expanded; the group collapses after `collapse_ms` of quiet. */
  touch(state, group) {
    const run = this.chain.then(() => this._touch(state, group));
    this.chain = run.catch((err) => this.log(`companion: ${err.message}`));
    return this.chain;
  }
  async _touch(state, group) {
    if (this.state === 'unsupported' || !group || !group.key) return;
    const tabId = await this.tabIdFor(state);
    if (tabId == null || this.userUngrouped.has(tabId)) return;
    let g = this.groups.get(group.key);
    if (!g) { g = { groupId: null, tabs: new Set(), timer: null }; this.groups.set(group.key, g); }
    const wasGrouped = g.tabs.has(tabId);
    let reply;
    try {
      reply = await this.call('herdrGroup', { tabId, groupId: g.groupId, title: String(group.title || group.key), color: String(group.color || 'purple'), wasGrouped });
    } catch (err) {
      if (/No tab with id/i.test(err.message)) { this.tabIds.delete(state.target); g.tabs.delete(tabId); }
      throw err;
    }
    if (reply && reply.skipped === 'user_ungrouped') { this.userUngrouped.add(tabId); g.tabs.delete(tabId); return; }
    if (!reply || reply.skipped) return;
    g.groupId = reply.groupId;
    g.tabs.add(tabId);
    if (g.timer) clearTimeout(g.timer);
    const ms = Math.max(1000, Number(group.collapse_ms) || 120000);
    g.timer = setTimeout(() => {
      g.timer = null;
      this.call('herdrCollapse', g.groupId).catch((err) => this.log(`companion collapse: ${err.message}`));
    }, ms);
    if (g.timer.unref) g.timer.unref();
  }
  /** Panes that are gone: their groups dissolve — the tabs we put in, and (by
   *  title, for a sidecar that restarted since) any group still carrying the name. */
  async release(entries) {
    for (const entry of entries) {
      const key = typeof entry === 'string' ? entry : String(entry.key || '');
      const title = typeof entry === 'string' ? null : entry.title;
      const g = this.groups.get(key);
      if (g) {
        if (g.timer) clearTimeout(g.timer);
        this.groups.delete(key);
        if (g.tabs.size) await this.call('herdrUngroup', [...g.tabs]).catch((err) => this.log(`companion release: ${err.message}`));
      }
      if (title) await this.call('herdrDissolve', String(title)).catch((err) => this.log(`companion dissolve: ${err.message}`));
    }
  }
  close() {
    for (const g of this.groups.values()) if (g.timer) clearTimeout(g.timer);
    this.groups.clear();
    this.tabIds.clear();
    if (this.ws) { try { this.ws.close(); } catch {} this.ws = null; }
  }
}
