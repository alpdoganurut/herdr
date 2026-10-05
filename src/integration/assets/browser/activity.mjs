// The activity overlay: the glow frame and the cursor injected into a page
// (an isolated world, a closed shadow root, no pointer events) while an agent
// works on it, and the companion extension's per-pane tab groups, driven over
// CDP. The look is the approved one from the overlay spike; its values are
// reused verbatim.

export const WORLD = 'herdr';
export const HOST_ATTR = 'data-herdr-overlay';
/** The companion worker code this sidecar expects (VERSION in companion/sw.js); an older running worker is reloaded. */
export const COMPANION_VERSION = 11;
/** The activity window when the directive names none (`linger_ms`, [browser] active_glyph_secs). */
export const LINGER_MS = 120000;
/** The frame fades this long after the agent's last act (or at the end of the window when that is shorter): steady through a burst of acts 5–20 s apart, gone soon after. */
export const FRAME_LINGER_MS = 15000;
/** The cursor hides this long after the last act that moved it (or at the end of the window when that is shorter); the frame stays. */
export const CURSOR_LINGER_MS = 5000;
/** The cursor's glide (matches the CSS transition). */
export const GLIDE_MS = 350;
/** The longest window a timer takes (Node's setTimeout limit; a longer delay would fire at once). */
export const MAX_TIMER_MS = 2 ** 31 - 1;
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

// Installed in the isolated world: `(OVERLAY_JS)(cursor, color, busy)`; idempotent
// per document, re-installed when the colour changed. `busy` is the look: the
// pulse while an operation runs, the calmer steady glow for the rest of the
// activity window. The alpha values are the approved look; only the rgb comes
// from the colour.
export const OVERLAY_JS = `((cursor, color, busy) => {
  const rgb = [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16)).join(',');
  const live = window.__herdrOverlay;
  if (live && live.alive && live.color === color) { live.show(); live.busy(busy); return 'present'; }
  if (live && live.alive) { cursor = live.cursor || cursor; live.alive = false; }
  document.querySelectorAll('div[${HOST_ATTR}]').forEach((n) => n.remove());
  const host = document.createElement('div');
  host.setAttribute('${HOST_ATTR}', '');
  host.setAttribute('data-herdr-color', color);
  host.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;opacity:0;transition:opacity .25s';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = \`<style>
    .frame{position:fixed;inset:0;border:2px solid rgba(\${rgb},.9);box-shadow:inset 0 0 14px 3px rgba(\${rgb},.42);
      border-radius:6px;animation:pulse 2s ease-in-out infinite;transition:box-shadow .6s,border-color .6s}
    .frame.idle{animation:none;border-color:rgba(\${rgb},.7);box-shadow:inset 0 0 10px 2px rgba(\${rgb},.28)}
    @keyframes pulse{50%{box-shadow:inset 0 0 22px 6px rgba(\${rgb},.62)}}
    .cur{position:fixed;left:-5px;top:-2.5px;width:24px;height:24px;transition:transform .35s cubic-bezier(.2,.8,.2,1),opacity .4s;
      filter:drop-shadow(0 1px 2px rgba(0,0,0,.5))}
    .ripple{position:fixed;width:28px;height:28px;margin:-14px 0 0 -14px;border-radius:50%;border:2px solid rgba(\${rgb},.9);
      animation:rip .5s ease-out forwards}
    @keyframes rip{from{transform:scale(.3);opacity:1}to{transform:scale(1.6);opacity:0}}
  </style><div class="frame\${busy ? '' : ' idle'}"></div>
  <svg class="cur" viewBox="0 0 24 24"><path d="M5 2.5v17.2l4.3-4.2 2.9 6.6 2.6-1.1-2.9-6.5h6.1z" fill="\${color}" stroke="#fff" stroke-width="1.4" stroke-linejoin="round"/></svg>\`;
  (document.documentElement || document).appendChild(host);
  const frame = root.querySelector('.frame');
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
    show() { if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; } host.style.transition = 'opacity .25s'; host.style.opacity = '1'; },
    busy(on) { frame.classList.toggle('idle', !on); },
    hide(fast) {
      // the window's end is a slow fade; a gone pane or a dismissed frame goes at once
      host.style.transition = fast ? 'opacity .25s' : 'opacity 1.2s';
      host.style.opacity = '0';
      hideTimer = setTimeout(() => { api.alive = false; host.remove(); if (window.__herdrOverlay === api) delete window.__herdrOverlay; }, fast ? 300 : 1300);
    },
    suspend(on) { host.style.visibility = on ? 'hidden' : ''; },
    moveTo(x, y, animate) {
      api.cursor = { x, y };
      cur.style.opacity = '1';
      if (!animate) cur.style.transition = 'none';
      cur.style.transform = 'translate(' + x + 'px,' + y + 'px)';
      if (!animate) { void cur.offsetWidth; cur.style.transition = ''; }
    },
    hideCursor() { cur.style.opacity = '0'; },
    probe() { return { frame: host.style.opacity, idle: frame.classList.contains('idle'), cursor: cur.style.opacity, hidden: host.style.visibility === 'hidden' }; },
    click(x, y) {
      const r = document.createElement('div');
      r.className = 'ripple'; r.style.left = x + 'px'; r.style.top = y + 'px';
      root.appendChild(r); setTimeout(() => r.remove(), 600);
    },
  };
  window.__herdrOverlay = api;
  return 'installed';
})`;

/** The overlay of one page: an isolated-world context per document, the
 *  cursor's last position, the two timers. The frame pulses while an op runs
 *  (`show`), settles to the calmer look when it is done (`linger`) and fades
 *  FRAME_LINGER_MS after the last act — every act re-shows it and restarts
 *  that — or at the end of the activity window (`linger_ms`,
 *  `[browser] active_glyph_secs`; the sidebar's ◎ and the group's ● keep the
 *  whole window) when that is shorter, or at once when the pane is gone
 *  (`dismiss`). The cursor hides on its own CURSOR_LINGER_MS after the last
 *  act that moved it (the window again the cap); a new document inside the
 *  window gets the frame back in the same state, the cursor only while it is
 *  still due (`reapply`). */
export class Overlay {
  constructor(state) {
    this.state = state;
    this.ctx = null;
    this.cursor = null;
    this.color = DEFAULT_COLOR;
    this.hideTimer = null;
    this.until = 0;
    this.active = false; // an op is running on the page
    this.cursorTimer = null;
    this.cursorUntil = 0; // Infinity between a move and the op's end
    this.cursorAt = 0; // when the last act moved the cursor
    this.frameLingerMs = FRAME_LINGER_MS; // (per instance so a check can shorten them)
    this.cursorLingerMs = CURSOR_LINGER_MS;
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
  /** Put the frame into the current document (idempotent; re-installed on a colour change), in the `busy` look or the idle one, the cursor where it was while it is still due. */
  install(busy) {
    const cursor = this.cursorUntil > Date.now() ? this.cursor : null;
    return this.eval(`(${OVERLAY_JS})(${JSON.stringify(cursor)}, ${JSON.stringify(this.color)}, ${busy ? 'true' : 'false'})`);
  }
  /** The activity window of a directive: as given, the default when missing or not a number, never past the timer's limit. */
  static window(ms) {
    const n = Number(ms);
    return Math.min(Number.isFinite(n) && n >= 0 ? n : LINGER_MS, MAX_TIMER_MS);
  }
  /** An op began: the frame is up and pulsing until `linger`. */
  async show(color) {
    if (color) this.color = normalizeColor(color);
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    this.active = true;
    this.until = Infinity;
    return this.install(true);
  }
  /** The op is done: the calmer look now; the frame fades FRAME_LINGER_MS from
   *  now and the cursor CURSOR_LINGER_MS from the act that moved it, each no
   *  later than the end of the window (`ms`, the directive's `linger_ms`; an
   *  explicit 0 means no linger at all). */
  linger(ms) {
    const window = Overlay.window(ms);
    const now = Date.now();
    const frame = Math.min(window, this.frameLingerMs);
    this.active = false;
    this.until = now + frame;
    this.eval('window.__herdrOverlay ? (__herdrOverlay.busy(false), "idle") : "absent"').catch(() => {});
    if (this.hideTimer) clearTimeout(this.hideTimer);
    this.hideTimer = setTimeout(() => {
      this.hideTimer = null;
      this.until = 0;
      this.eval('window.__herdrOverlay ? (__herdrOverlay.hide(false), "hidden") : "absent"').catch(() => {});
    }, frame);
    if (this.hideTimer.unref) this.hideTimer.unref();
    // the cursor: scheduled once per move, from the move; an op that did not move it leaves the timer be
    if (this.cursorUntil === Infinity) {
      const due = Math.max(0, this.cursorAt + Math.min(window, this.cursorLingerMs) - now);
      this.cursorUntil = Math.min(now + due, this.until);
      if (this.cursorTimer) clearTimeout(this.cursorTimer);
      this.cursorTimer = setTimeout(() => {
        this.cursorTimer = null;
        this.cursorUntil = 0;
        this.eval('window.__herdrOverlay ? (__herdrOverlay.hideCursor(), "cursor hidden") : "absent"').catch(() => {});
      }, Math.min(due, frame));
      if (this.cursorTimer.unref) this.cursorTimer.unref();
    }
  }
  /** The pane is gone: the frame and the cursor go now, whatever is left of the window. */
  dismiss() {
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    if (this.cursorTimer) { clearTimeout(this.cursorTimer); this.cursorTimer = null; }
    const up = this.until > Date.now();
    this.active = false;
    this.until = 0;
    this.cursorUntil = 0;
    if (!up) return Promise.resolve();
    return this.eval('window.__herdrOverlay ? (__herdrOverlay.hide(true), "hidden") : "absent"').catch(() => {});
  }
  /** Glide the cursor to a viewport point (and wait for the glide when animating); it shows until CURSOR_LINGER_MS after the op ends. */
  async moveTo(x, y, animate) {
    this.cursor = { x, y };
    this.cursorAt = Date.now();
    this.cursorUntil = Infinity;
    if (this.cursorTimer) { clearTimeout(this.cursorTimer); this.cursorTimer = null; }
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
  /** A new document while the frame should be up: put it back in the same state (the window is not stretched). */
  async reapply() {
    if (this.until > Date.now()) await this.install(this.active).catch((err) => this.log(`overlay reapply: ${err.message}`));
  }
  dispose() {
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    if (this.cursorTimer) { clearTimeout(this.cursorTimer); this.cursorTimer = null; }
    this.active = false;
    this.until = 0;
    this.cursorUntil = 0;
  }
}

/** Panes that are gone: the overlay of every page a pane's directive touched
 *  (`state.paneKey`, recorded by the host when the directive arrived) goes
 *  now — whatever the companion's state, grouped or not. Answers how many. */
export function dismissPanes(profiles, keys) {
  const wanted = new Set(keys.map(String));
  let dismissed = 0;
  for (const profile of profiles) {
    if (!profile.pages) continue;
    for (const state of profile.pages.values()) {
      if (!state.closed && state.paneKey && wanted.has(state.paneKey) && state.overlay) {
        state.overlay.dismiss();
        dismissed++;
      }
    }
  }
  return dismissed;
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
    this.groups = new Map(); // pane key -> { tabs: Set<tabId>, timer } (the group id lives in the worker's session storage)
    this.userUngrouped = new Set();
    this.chain = Promise.resolve();
    this.connecting = null;
    this.log = () => {};
    this.stale = false; // the running worker is older than this herdr's files
    this.scope = null; // chrome-extension://<id>/
    this.gen = 0; // bumped by close(): a worker wait from an earlier browser gives up
  }
  info() { return { state: this.state, detail: this.detail }; }
  /** A DevTools session on a page of the profile over a connection of its
   *  own: the `ServiceWorker` domain is only served on page targets (not on
   *  the browser target), and an unregister sent through the sidecar's
   *  long-lived connection never took while the same calls over a fresh
   *  `connectOverCDP` did (live, repeatedly). Answers { session, close }. */
  async pageSession() {
    // (loaded here, not at the top: scripts/test_browser_companion.mjs imports this file from the source tree)
    const { chromium } = await import('playwright-core');
    const browser = await chromium.connectOverCDP(`http://127.0.0.1:${this.profile.port}`, { timeout: COMPANION_CALL_MS });
    const close = () => browser.close().catch(() => {});
    const ctx = browser.contexts()[0];
    const pages = ctx ? ctx.pages() : [];
    // a page outside the extension's own scope first (what worked live)
    const page = pages.find((p) => !this.scope || !p.url().startsWith(this.scope)) || pages[0];
    if (!page) { await close(); return null; }
    const session = await ctx.newCDPSession(page);
    return { session, close };
  }
  /** Chrome keeps an unpacked command-line extension's worker script for good:
   *  a manifest version bump, a browser restart, `chrome.runtime.reload()`
   *  and `importScripts` of a new URL do not refresh it, and a worker that is
   *  unregistered at runtime does not come back until the next browser start.
   *  So a stale worker keeps serving this session (reported as pending), and
   *  right before herdr closes the browser its registration is removed and
   *  the worker stopped — together, in one DevTools session: an unregister
   *  alone never completed while the worker ran, and a worker that was still
   *  running at shutdown came back with the old script. After that the next
   *  start registers the files on disk afresh (verified live). The
   *  `ServiceWorker` domain is only served on page targets, hence the page
   *  session; nothing on disk is touched. */
  async stopStaleWorker() {
    if (!this.stale || !this.scope) return false;
    const link = await this.pageSession();
    if (!link) { this.log('companion: no page for a DevTools session; the stale worker stays'); return false; }
    const { session } = link;
    const versions = new Map(); // versionId -> runningStatus
    let deleted = false;
    const onVersions = ({ versions: list }) => { for (const v of list) if (v.scriptURL.startsWith(this.scope)) versions.set(v.versionId, v.runningStatus); };
    const onRegistrations = ({ registrations }) => { for (const r of registrations) if (r.scopeURL === this.scope && r.isDeleted) deleted = true; };
    const live = () => [...versions].filter(([, status]) => status === 'running' || status === 'starting').map(([id]) => id);
    session.on('ServiceWorker.workerVersionUpdated', onVersions);
    session.on('ServiceWorker.workerRegistrationUpdated', onRegistrations);
    try {
      await withTimeout(session.send('ServiceWorker.enable'), COMPANION_CALL_MS, 'ServiceWorker.enable');
      await sleep(700); // the domain's initial dump settles (an unregister sent right after enable never took)
      const running = live();
      // (the held worker connection stays open until the worker is gone: a
      // registration whose worker lost its DevTools session first was never
      // marked deleted)
      await withTimeout(session.send('ServiceWorker.unregister', { scopeURL: this.scope }), COMPANION_CALL_MS, 'ServiceWorker.unregister');
      for (const versionId of running) await session.send('ServiceWorker.stopWorker', { versionId }).catch(() => {});
      for (let i = 0; i < 20 && !(deleted && running.every((id) => versions.get(id) === 'stopped')); i++) await sleep(100);
      this.close();
      this.log(`companion: stale worker retired (registration ${deleted ? 'deleted' : 'still listed'}, ${running.length} stopped); the next browser start loads the new files`);
      return deleted;
    } finally {
      session.off('ServiceWorker.workerVersionUpdated', onVersions);
      session.off('ServiceWorker.workerRegistrationUpdated', onRegistrations);
      await session.detach().catch(() => {});
      await link.close();
    }
  }
  async targets() {
    const res = await fetch(`http://127.0.0.1:${this.profile.port}/json/list`, { signal: AbortSignal.timeout(2000) });
    return res.json();
  }
  findIn(list) { return list.find((t) => t.type === 'service_worker' && /\/sw\.js$/.test(t.url)) || null; }
  /** Is the extension loaded, and current? (at attach) A worker still running
   *  older code (the files were refreshed under it) keeps serving this
   *  session and is reported as pending; see `stopStaleWorker`. */
  async probe() {
    this.stale = false;
    this.scope = null;
    if (typeof WebSocket !== 'function') { this.state = 'unsupported'; this.detail = 'node has no WebSocket'; return this.info(); }
    try {
      const sw = await this.worker(true);
      if (!sw) { this.state = 'missing'; this.detail = 'no companion service worker on the DevTools port'; return this.info(); }
      this.state = 'ready';
      this.detail = '';
      // (not URL.origin: Node answers "null" for chrome-extension: URLs)
      this.scope = sw.url.slice(0, sw.url.lastIndexOf('/') + 1);
      const ping = String(await this.call('herdrPing').catch(() => ''));
      const version = Number((ping.split('/')[1] || '0'));
      if (version !== COMPANION_VERSION) {
        this.stale = true; // retired by stopStaleWorker when herdr closes the browser
        this.detail = `worker v${version || '?'}, this herdr expects v${COMPANION_VERSION} — extension update pending: \`herdr browser stop\` and open again`;
      }
    } catch (err) {
      this.state = 'missing';
      this.detail = String(err.message).slice(0, 120);
    }
    return this.info();
  }
  /** The worker target, woken by a tab event when Chrome idled it out. */
  async worker(wake) {
    const gen = this.gen;
    let sw = this.findIn(await this.targets());
    if (!sw && wake) {
      await this.nudge();
      for (let i = 0; i < 15 && !sw && gen === this.gen && this.profile.browser; i++) { await sleep(100); sw = this.findIn(await this.targets()); }
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
  /** One connection at a time: concurrent callers share the in-flight attempt. */
  connect() {
    if (this.ws && this.ws.readyState === 1) return Promise.resolve(this.ws);
    if (this.connecting) return this.connecting;
    this.connecting = this._connect().finally(() => { this.connecting = null; });
    return this.connecting;
  }
  async _connect() {
    // A worker Chrome idled out between the keep-alive alarms is a transient
    // miss: the call fails, `state` stays what `probe` found, the next call
    // retries (a latched 'missing' here once turned every group and page
    // push off until the next attach).
    const sw = await this.worker(true);
    if (!sw) throw new Error('companion: no service worker right now (idle?); retrying on the next call');
    const ws = new WebSocket(sw.webSocketDebuggerUrl);
    try {
      await withTimeout(new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('companion: websocket failed')); }), COMPANION_CALL_MS, 'companion connect');
    } catch (err) {
      try { ws.close(); } catch {}
      throw err;
    }
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
  async tabIdFor(state, quiet = false) {
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
    if (candidates.length !== 1) { if (!quiet) this.log(`companion: ${candidates.length} tabs match ${url}; not grouped`); return null; }
    this.tabIds.set(state.target, candidates[0].id);
    return candidates[0].id;
  }
  forget(targetId) {
    const tabId = this.tabIds.get(targetId);
    this.tabIds.delete(targetId);
    if (tabId != null) for (const g of this.groups.values()) g.tabs.delete(tabId);
  }
  /** An agent pane touched a tab: it sits in the pane's group, expanded and
   *  marked active in its title (`● <title>`, by the worker); after
   *  `collapse_ms` of quiet the group collapses and the mark goes. */
  touch(state, group) {
    const run = this.chain.then(() => this._touch(state, group));
    this.chain = run.catch((err) => this.log(`companion: ${err.message}`));
    return this.chain;
  }
  async _touch(state, group) {
    if (this.state === 'unsupported' || !group || !group.key) return;
    // The directive's window as it is; missing or not a number: the default.
    // 0 is off: no group expanded or marked for the pane, no collapse later
    // (and, without a companion flag for "join quietly", no grouping either).
    const ms = Overlay.window(group.collapse_ms);
    if (ms === 0) return;
    const tabId = await this.tabIdFor(state);
    if (tabId == null || this.userUngrouped.has(tabId)) return;
    let g = this.groups.get(group.key);
    if (!g) { g = { tabs: new Set(), timer: null }; this.groups.set(group.key, g); }
    const wasGrouped = g.tabs.has(tabId);
    let reply;
    try {
      // The group is the pane's (keyed by its id in the worker's session storage), never one found by title.
      reply = await this.call('herdrGroup', { key: String(group.key), tabId, title: String(group.title || group.key), color: String(group.color || 'purple'), wasGrouped, active: true });
    } catch (err) {
      if (/No tab with id/i.test(err.message)) { this.tabIds.delete(state.target); g.tabs.delete(tabId); }
      throw err;
    }
    if (reply && reply.skipped === 'user_ungrouped') { this.userUngrouped.add(tabId); g.tabs.delete(tabId); return; }
    if (!reply || reply.skipped) return;
    g.tabs.add(tabId);
    if (g.timer) clearTimeout(g.timer);
    g.timer = setTimeout(() => {
      g.timer = null;
      // (a stale v10 worker reads a bare key only; it keeps the plain title anyway)
      const arg = this.stale ? String(group.key) : { key: String(group.key), title: String(group.title || group.key) };
      this.call('herdrCollapse', arg).catch((err) => this.log(`companion collapse: ${err.message}`));
    }, ms);
    if (g.timer.unref) g.timer.unref();
  }
  /** The pinned dashboard: on in every normal window, or removed. */
  dashboard(pin) {
    return this.call('herdrDashboard', { pin: Boolean(pin) });
  }
  /** Panes that are gone: the overlay on each of the pane's tabs goes now and
   *  the group recorded for each key dissolves (only that group; a same-named
   *  group of the user's or another pane's is never touched). Throws when the
   *  worker could not do it. */
  async release(keys) {
    for (const key of keys) {
      const g = this.groups.get(String(key));
      if (g) {
        if (g.timer) clearTimeout(g.timer);
        const pages = this.profile.pages;
        if (pages) {
          for (const [target, tabId] of this.tabIds) {
            const state = g.tabs.has(tabId) ? pages.get(target) : null;
            if (state && state.overlay) state.overlay.dismiss();
          }
        }
        this.groups.delete(String(key));
      }
      await this.call('herdrRelease', String(key));
    }
  }
  /** The browser went away (or is being closed): drop everything that
   *  belonged to it. An in-flight connect keeps failing on its own; the next
   *  attach's calls must not wait on it (one probe timed out that way). */
  close() {
    for (const g of this.groups.values()) if (g.timer) clearTimeout(g.timer);
    this.groups.clear();
    this.tabIds.clear();
    if (this.ws) { try { this.ws.close(); } catch {} this.ws = null; }
    this.connecting = null;
    this.gen++;
    this.stale = false;
    this.scope = null;
  }
}
