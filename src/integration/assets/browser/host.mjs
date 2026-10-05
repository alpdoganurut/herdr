// herdr browser sidecar: one Node process per herdr server, JSON lines on stdio.
// Attaches to herdr-launched Chromium over CDP with playwright-core
// (connectOverCDP, noDefaults) and runs read operations on its tabs. Holds no
// durable state: ring buffers and refs are "since attach".
//
// Request  {"id":1,"op":"read","profile":"main","target":"<targetId>","args":{...},"deadline_ms":30000,
//           "activity":{"frame":true,"animate":true,"color":"#aa6eff","linger_ms":120000,"group":{"key":"w2:p7","title":"✻ planner","color":"purple","collapse_ms":120000}}}
//          (activity: an agent pane's call; the frame/cursor overlay — up for linger_ms after the op — and the tab group; absent for the user)
// Reply    {"id":1,"ok":true,"result":{...},"page":{"url":"…","title":"…","dialog_open":false}}
//          {"id":1,"ok":false,"error":{"code":"…","message":"…"}}
// Events   {"event":"tab"|"dialog"|"browser"|"log", ...}
import { chromium } from 'playwright-core';
import { createRequire } from 'node:module';
import readline from 'node:readline';
import fs from 'node:fs';
import { EXTRACT_SOURCE, LINKS_SOURCE } from './extract.mjs';
import { Overlay, Companion, NUDGE_URL, dismissPanes, captureWithQuietRetry, SELECT_BUDGET_MS } from './activity.mjs';

const require = createRequire(import.meta.url);
// Real functions for page.evaluate: a string would be evaluated as an expression (yielding the function,
// not its result) and an in-page eval would hit strict CSPs.
const extractFn = new Function('args', 'return (' + EXTRACT_SOURCE + ')(args);');
const linksFn = new Function('args', 'return (' + LINKS_SOURCE + ')(args);');
const PLAYWRIGHT_VERSION = require('playwright-core/package.json').version;
const HOST_PROTOCOL = 1;
const CONSOLE_RING = 1000;
const NETWORK_RING = 500;
const TEXT_CAP = 2048;
const HTML_CAP = 200 * 1024;
const SCREENSHOT_STALL_MS = 5000;

const profiles = new Map(); // name -> Profile

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}
function event(name, fields) {
  send(Object.assign({ event: name }, fields));
}
function log(level, text) {
  event('log', { level, text: String(text).slice(0, 500) });
}
class OpError extends Error {
  constructor(code, message) { super(message); this.code = code; }
}
const fail = (code, message) => { throw new OpError(code, message); };
const sleep = (ms) => new Promise(r => setTimeout(r, ms));
function withTimeout(promise, ms, code, message) {
  let timer;
  const timeout = new Promise((_, reject) => { timer = setTimeout(() => reject(new OpError(code, message)), ms); });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

class PageState {
  constructor(profile, page, target) {
    this.profile = profile;
    this.page = page;
    this.target = target;
    this.console = [];
    this.consoleSeq = 0;
    this.network = [];
    this.networkSeq = 0;
    this.requests = new Map();
    this.dialog = null;
    this.inflight = 0;
    this.closed = false;
    this.paneKey = null; // the agent pane whose directive last touched this page
    this.overlay = new Overlay(this);
    this.overlay.log = (text) => log('debug', text);
  }
  pushConsole(entry) {
    entry.seq = ++this.consoleSeq;
    this.console.push(entry);
    if (this.console.length > CONSOLE_RING) this.console.shift();
  }
  pushNetwork(entry) {
    entry.seq = ++this.networkSeq;
    this.network.push(entry);
    if (this.network.length > NETWORK_RING) this.network.shift();
  }
}

class Profile {
  constructor(name) {
    this.name = name;
    this.browser = null;
    this.ctx = null;
    this.port = null;
    this.pages = new Map(); // targetId -> PageState
    this.pending = new Map(); // page -> Promise<PageState> while the target id is looked up
    this.companion = new Companion(this);
    this.companion.log = (text) => log('debug', text);
    this.chain = Promise.resolve();
  }
  // One at a time per profile (tab creation and its extension-tab adoption must not interleave).
  serial(fn) {
    const run = this.chain.then(fn, fn);
    this.chain = run.catch(() => {});
    return run;
  }
}

// ---------------------------------------------------------------------------
// Attach and tracking

async function targetIdOf(profile, page) {
  const session = await profile.ctx.newCDPSession(page);
  try {
    const { targetInfo } = await session.send('Target.getTargetInfo');
    return { targetId: targetInfo.targetId, session };
  } catch (err) {
    await session.detach().catch(() => {});
    throw err;
  }
}

async function track(profile, page, initiator) {
  if (page.url() === NUDGE_URL) return null; // the companion's wake-up tab: never a herdr tab
  if (profile.pending.has(page)) return profile.pending.get(page);
  const promise = (async () => {
    const { targetId, session } = await targetIdOf(profile, page);
    const state = new PageState(profile, page, targetId);
    profile.pages.set(targetId, state);
    state.session = session;
    // Page events are per session: without Page.enable on ours, javascriptDialogClosed never arrives.
    await session.send('Page.enable').catch(() => {});
    session.on('Page.javascriptDialogClosed', () => {
      if (state.dialog) {
        state.dialog = null;
        event('dialog', { profile: profile.name, target: targetId, state: 'closed' });
      }
    });
    page.on('console', (msg) => {
      const loc = msg.location() || {};
      const level = msg.type();
      state.pushConsole({ ts: Date.now(), level, text: msg.text().slice(0, TEXT_CAP), url: loc.url || '', line: loc.lineNumber });
      if (level === 'error') event('tab', { profile: profile.name, target: targetId, kind: 'console_error' });
    });
    page.on('pageerror', (err) => {
      state.pushConsole({ ts: Date.now(), level: 'pageerror', text: String(err && err.message || err).slice(0, TEXT_CAP), url: '', line: undefined });
      event('tab', { profile: profile.name, target: targetId, kind: 'console_error' });
    });
    page.on('request', (req) => { state.requests.set(req, { start: Date.now() }); });
    page.on('requestfinished', async (req) => {
      const started = state.requests.get(req); state.requests.delete(req);
      let status = null; let bytes = 0;
      try { const res = await req.response(); status = res ? res.status() : null; const sizes = await req.sizes().catch(() => null); bytes = sizes ? sizes.responseBodySize : 0; } catch {}
      state.pushNetwork({ ts: Date.now(), method: req.method(), url: req.url().slice(0, TEXT_CAP), type: req.resourceType(), status, failed: false, failure: null, duration_ms: started ? Date.now() - started.start : 0, bytes });
    });
    page.on('requestfailed', (req) => {
      const started = state.requests.get(req); state.requests.delete(req);
      const failure = req.failure();
      state.pushNetwork({ ts: Date.now(), method: req.method(), url: req.url().slice(0, TEXT_CAP), type: req.resourceType(), status: null, failed: true, failure: failure ? failure.errorText : 'failed', duration_ms: started ? Date.now() - started.start : 0, bytes: 0 });
    });
    page.on('framenavigated', (frame) => {
      if (frame !== page.mainFrame()) return;
      state.overlay.navigated();
      event('tab', { profile: profile.name, target: targetId, kind: 'navigated', url: frame.url(), title: '', initiator: state.inflight > 0 ? 'command' : 'other' });
    });
    // A new document while the frame should be up: the overlay comes back with it.
    page.on('domcontentloaded', () => { state.overlay.reapply().catch(() => {}); });
    page.on('load', async () => {
      const title = await page.title().catch(() => '');
      if (title) event('tab', { profile: profile.name, target: targetId, kind: 'title', title });
    });
    page.on('close', () => {
      state.closed = true;
      state.overlay.dispose();
      profile.companion.forget(targetId);
      profile.pages.delete(targetId);
      profile.pending.delete(page);
      event('tab', { profile: profile.name, target: targetId, kind: 'closed' });
    });
    page.on('crash', () => event('tab', { profile: profile.name, target: targetId, kind: 'crashed' }));
    page.on('response', (res) => { if (res.request().isNavigationRequest() && res.frame() === page.mainFrame()) state.lastStatus = res.status(); });
    if (initiator) {
      event('tab', { profile: profile.name, target: targetId, kind: 'opened', url: page.url(), title: '', initiator });
    }
    return state;
  })();
  profile.pending.set(page, promise);
  promise.catch(() => profile.pending.delete(page));
  return promise;
}

async function attach(profile, port, deadlineMs, pinDashboard) {
  if (profile.browser && profile.browser.isConnected()) {
    return tabs(profile);
  }
  profile.port = port;
  const t0 = Date.now();
  const stamp = () => `attach ${profile.name}:${port}`;
  log('debug', `${stamp()} connectOverCDP…`);
  let browser;
  try {
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true, timeout: Math.max(1000, deadlineMs - 500) });
  } catch (err) {
    const message = String(err && err.message || err).split('\n')[0];
    log('debug', `${stamp()} connectOverCDP failed after ${Date.now() - t0} ms: ${message}`);
    // A distinct code for the connect timeout: herdr retries that once.
    if (/Timeout/i.test(message)) fail('attach_timeout', `the browser did not accept the CDP connection within ${deadlineMs} ms (${message})`);
    throw err;
  }
  log('debug', `${stamp()} connected in ${Date.now() - t0} ms, pages ${browser.contexts()[0] ? browser.contexts()[0].pages().length : 'none'}`);
  profile.browser = browser;
  const ctx = browser.contexts()[0];
  if (!ctx) fail('browser_start_failed', 'the browser has no default context');
  profile.ctx = ctx;
  // No-op recording dialog listener: without any listener Playwright auto-dismisses
  // every dialog, including the user's. With it the dialog stays open for the user.
  ctx.on('dialog', async (dialog) => {
    const page = dialog.page();
    let state = null;
    for (const s of profile.pages.values()) if (s.page === page) state = s;
    if (!state) { try { state = await track(profile, page, null); } catch { return; } }
    state.dialog = dialog;
    event('dialog', { profile: profile.name, target: state.target, type: dialog.type(), message: dialog.message().slice(0, 500), state: 'open' });
  });
  ctx.on('page', (page) => {
    track(profile, page, 'other').catch((err) => log('warn', `track failed: ${err.message}`));
  });
  browser.on('disconnected', () => {
    profile.browser = null;
    profile.ctx = null;
    profile.companion.close();
    for (const [target] of profile.pages) profile.pages.delete(target);
    event('browser', { profile: profile.name, kind: 'disconnected', detail: 'CDP connection closed' });
  });
  await Promise.all(ctx.pages().map(page => track(profile, page, null).catch((err) => log('warn', `track failed: ${err.message}`))));
  log('debug', `${stamp()} tracked in ${Date.now() - t0} ms`);
  event('browser', { profile: profile.name, kind: 'attached', detail: browser.version() });
  // The companion extension (tab groups): loaded or not, bounded.
  const companion = await withTimeout(profile.companion.probe(), 6000, 'x', 'x').catch(() => ({ state: 'missing', detail: 'probe timed out' }));
  log('debug', `${stamp()} companion ${companion.state}${companion.detail ? ' (' + companion.detail + ')' : ''}`);
  if (companion.state === 'ready' && pinDashboard !== undefined) {
    await withTimeout(profile.companion.dashboard(pinDashboard), 3000, 'x', 'x').catch((err) => log('debug', `dashboard: ${err.message}`));
  }
  return Object.assign(await tabs(profile), { companion });
}

async function tabs(profile) {
  const out = [];
  for (const state of profile.pages.values()) {
    if (state.closed) continue;
    let selected = false;
    let title = '';
    if (!state.dialog) {
      selected = await withTimeout(state.page.evaluate(() => document.visibilityState === 'visible'), 500, 'x', 'x').catch(() => false);
      title = await withTimeout(state.page.title(), 500, 'x', 'x').catch(() => '');
    }
    out.push({ target: state.target, url: state.page.url(), title, selected, dialog_open: Boolean(state.dialog) });
  }
  return { tabs: out };
}

function needProfile(name) {
  const profile = profiles.get(name);
  if (!profile || !profile.browser || !profile.browser.isConnected()) fail('browser_host_restarted', `profile ${name} is not attached; retry`);
  return profile;
}
function needPage(profile, target) {
  const state = profile.pages.get(target);
  if (!state || state.closed) fail('tab_closed', 'that browser tab is gone; open another or pick one with browser tabs');
  return state;
}
// A dialog the user answered in the window: the renderer answers again, so a quick evaluate
// resolving means the dialog is gone (belt and braces next to Page.javascriptDialogClosed).
async function dialogStillOpen(state) {
  if (!state.dialog) return false;
  const gone = await withTimeout(state.page.evaluate('1').then(() => true), 300, 'x', 'x').catch(() => false);
  if (gone) {
    state.dialog = null;
    event('dialog', { profile: state.profile.name, target: state.target, state: 'closed' });
  }
  return Boolean(state.dialog);
}
/** The companion's quiet tab selection for a page: true when it happened (never raises the window). */
async function quietSelect(state) {
  const companion = state.profile.companion;
  if (!companion || companion.state !== 'ready') return false;
  return withTimeout(companion.select(state), SELECT_BUDGET_MS, 'x', 'x').catch((err) => { log('debug', `quiet select: ${err.message}`); return false; });
}
async function pageInfo(state) {
  const dialogOpen = await dialogStillOpen(state);
  let title = '';
  if (!dialogOpen) title = await withTimeout(state.page.title(), 800, 'x', 'x').catch(() => '');
  return { url: state.page.url(), title, dialog_open: dialogOpen, status: state.lastStatus || null };
}
function waitUntilOf(wait) {
  if (wait === undefined || wait === null || wait === '') return 'domcontentloaded';
  if (wait === 'domcontentloaded' || wait === 'load' || wait === 'networkidle' || wait === 'commit') return wait;
  fail('invalid_request', `unknown wait state ${JSON.stringify(wait)}; expected domcontentloaded, load or networkidle`);
}
async function settle(state, wait) {
  if (wait === 'networkidle' || wait === 'load') return;
  // domcontentloaded + up to 1.5 s of network quiet, best effort
  await state.page.waitForLoadState('networkidle', { timeout: 1500 }).catch(() => {});
}
// Retry once when the page navigated mid-op (user or script).
const isNavigationError = (err) => /Execution context was destroyed|navigation|detached/i.test(err.message);
async function withRetry(state, fn) {
  try {
    return { value: await fn(), navigated_during: false };
  } catch (err) {
    if (isNavigationError(err)) {
      await state.page.waitForLoadState('domcontentloaded', { timeout: 5000 }).catch(() => {});
      return { value: await fn(), navigated_during: true };
    }
    throw err;
  }
}
function scopeLocator(state, args) {
  if (args.ref) return state.page.locator(`aria-ref=${args.ref}`);
  if (args.selector) return state.page.locator(args.selector).first();
  return null;
}
async function guardDialog(state) {
  if (await dialogStillOpen(state)) fail('dialog_open', `a ${state.dialog.type()} dialog is open on this tab ("${state.dialog.message().slice(0, 80)}"); browser dialog accept|dismiss, or the user answers it`);
}

// ---------------------------------------------------------------------------
// Operations

async function createTarget(profile, url, background) {
  const session = await profile.browser.newBrowserCDPSession();
  try {
    try {
      return (await session.send('Target.createTarget', { url, background })).targetId;
    } catch (err) {
      if (/window/i.test(err.message)) {
        return (await session.send('Target.createTarget', { url, background, newWindow: true })).targetId;
      }
      throw err;
    }
  } finally {
    await session.detach().catch(() => {});
  }
}

const ops = {
  async hello() {
    return { host_protocol: HOST_PROTOCOL, playwright_version: PLAYWRIGHT_VERSION, node_version: process.version };
  },
  async ping() { return {}; },

  async attach({ profile: name, args, deadline_ms }) {
    let profile = profiles.get(name);
    if (!profile) { profile = new Profile(name); profiles.set(name, profile); }
    return attach(profile, args.port, deadline_ms, args.pin_dashboard);
  },
  async detach({ profile: name }) {
    const profile = profiles.get(name);
    if (profile && profile.browser) await profile.browser.close().catch(() => {});
    profiles.delete(name);
    return {};
  },
  async close_browser({ profile: name }) {
    const profile = needProfile(name);
    // A stale companion worker is stopped first, so the next start registers the new files.
    // (herdr waits STOP_GRACE = 5 s for this op; the retire needs up to ~3 s on a slow path)
    await withTimeout(profile.companion.stopStaleWorker(), 4000, 'x', 'x').catch((err) => log('debug', `stale worker stop: ${err.message}`));
    const session = await profile.browser.newBrowserCDPSession();
    await session.send('Browser.close').catch(() => {});
    return {};
  },
  async tabs({ profile: name }) {
    return tabs(needProfile(name));
  },

  async open({ profile: name, args, deadline_ms, activity }) {
    const profile = needProfile(name);
    const waitUntil = waitUntilOf(args.wait);
    // A blank target first: the page is tracked (listeners on, the first
    // document's status included) before the navigation starts. An agent's
    // tab is adopted by the companion (its extension tab id) while it is
    // still the newest blank tab.
    const targetId = await profile.serial(async () => {
      const id = await createTarget(profile, 'about:blank', args.background !== false);
      if (activity && activity.group && profile.companion.state === 'ready') {
        await withTimeout(profile.companion.adoptNew(id), 1500, 'x', 'x').catch((err) => log('debug', `companion adopt: ${err.message}`));
      }
      return id;
    });
    const deadline = Date.now() + Math.min(deadline_ms, 10000);
    let state = profile.pages.get(targetId);
    while (!state && Date.now() < deadline) { await sleep(50); state = profile.pages.get(targetId); }
    if (!state) fail('browser_start_failed', 'the new tab did not show up');
    state.inflight++;
    try {
      const response = await state.page.goto(args.url, { waitUntil, timeout: Math.max(1000, deadline_ms - 1000) })
        .catch((err) => {
          // The tab exists and is the caller's now: the error names it.
          const failure = new OpError(/Timeout/i.test(err.message) ? 'browser_timeout' : 'navigation_failed', err.message.split('\n')[0] + ' (the new tab is your current tab; browser read shows what loaded)');
          failure.target = targetId;
          throw failure;
        });
      if (response) state.lastStatus = response.status();
      await settle(state, args.wait);
    } finally { state.inflight--; }
    let card = null;
    if (!state.dialog) {
      const r = await withRetry(state, () => state.page.evaluate(extractFn, {})).catch(() => null);
      if (r && r.value && !r.value.error) {
        const x = r.value;
        card = { headings: x.headings, links: x.links, forms: x.forms, text_chars: x.text_chars, main_chars: x.main_chars, login_wall: x.login_wall, preview: x.markdown.slice(0, 1500) };
      }
    }
    return { result: { target: targetId, card }, page: await pageInfo(state) };
  },

  async navigate({ profile: name, target, args, deadline_ms }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    state.inflight++;
    let response = null;
    try {
      response = await state.page.goto(args.url, { waitUntil: waitUntilOf(args.wait), timeout: Math.max(1000, deadline_ms - 500) })
        .catch((err) => { throw new OpError(/Timeout/i.test(err.message) ? 'browser_timeout' : 'navigation_failed', err.message.split('\n')[0] + ' (page partially loaded; try browser read)'); });
      await settle(state, args.wait);
    } finally { state.inflight--; }
    state.lastStatus = response ? response.status() : null;
    return { result: {}, page: await pageInfo(state) };
  },

  async history({ profile: name, target, args, deadline_ms }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    state.inflight++;
    let response = null;
    try {
      const opts = { waitUntil: 'domcontentloaded', timeout: Math.max(1000, deadline_ms - 500) };
      if (args.action === 'back') response = await state.page.goBack(opts);
      else if (args.action === 'forward') response = await state.page.goForward(opts);
      else response = await state.page.reload(opts);
      await settle(state, null);
    } catch (err) {
      throw new OpError(/Timeout/i.test(err.message) ? 'browser_timeout' : 'navigation_failed', err.message.split('\n')[0]);
    } finally { state.inflight--; }
    if (response) state.lastStatus = response.status();
    return { result: { moved: Boolean(response) || args.action === 'reload' }, page: await pageInfo(state) };
  },

  async close({ profile: name, target }) {
    const state = needPage(needProfile(name), target);
    await state.page.close({ runBeforeUnload: false });
    return {};
  },

  // The one op that raises the window: the login handoff to the user
  // (`browser focus`). Everything else selects tabs quietly (`select`).
  async focus({ profile: name, target }) {
    const state = needPage(needProfile(name), target);
    await state.page.bringToFront();
    return { result: {}, page: await pageInfo(state) };
  },

  // The tab becomes the active tab of its window without the app coming
  // forward (the companion's chrome.tabs.update); `--front` / `open --focus`
  // and the screenshot retry use it. No companion: no raise either, a clear
  // error.
  async select({ profile: name, target }) {
    const state = needPage(needProfile(name), target);
    const selected = await quietSelect(state);
    if (!selected) fail('tab_not_selected', 'the tab could not be selected quietly: the companion extension is not ready or does not know this tab; the window was not raised — `browser focus` brings it up for the user');
    return { result: {}, page: await pageInfo(state) };
  },

  async read({ profile: name, target, args, deadline_ms }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    const scope = scopeLocator(state, args);
    const format = args.format || 'markdown';
    const run = async () => {
      if (format === 'snapshot') {
        const loc = scope || state.page.locator('body');
        let snap = await loc.ariaSnapshot({ mode: 'ai', timeout: Math.max(1000, deadline_ms - 500) });
        if (args.interactive) snap = interactiveOnly(snap);
        return snap;
      }
      if (format === 'text') {
        if (scope) return await scope.innerText({ timeout: 5000 });
        return await state.page.evaluate(() => document.body ? document.body.innerText : '');
      }
      if (format === 'html') {
        const html = scope ? await scope.evaluate(el => el.outerHTML, undefined, { timeout: 5000 }) : await state.page.content();
        if (html.length > HTML_CAP && !scope) fail('too_large', `the page HTML is ${html.length} chars; pass --selector or --ref to read a part, or use markdown`);
        return html;
      }
      // markdown
      if (args.ref) {
        const handle = await scope.elementHandle({ timeout: 5000 });
        if (!handle) fail('stale_ref', `ref ${args.ref} no longer resolves; take a new snapshot`);
        const r = await state.page.evaluate(extractFn, { element: handle });
        if (!r || r.error) fail('invalid_request', (r && r.error) || 'extraction failed');
        return r.markdown;
      }
      const r = await state.page.evaluate(extractFn, { selector: args.selector || null });
      if (!r || r.error) fail('invalid_request', (r && r.error) || 'extraction failed');
      return (args.selector ? r.markdown : (r.markdown && r.markdown.length >= 200 ? r.markdown : r.full_markdown));
    };
    let outcome;
    try {
      outcome = await withRetry(state, run);
    } catch (err) {
      if (err instanceof OpError) throw err;
      if (/aria-ref/i.test(err.message) || (args.ref && /Timeout/i.test(err.message))) fail('stale_ref', `ref ${args.ref} no longer resolves (the page changed); take a new snapshot`);
      if (/Timeout/i.test(err.message) && args.selector) fail('invalid_request', `selector ${args.selector} matched nothing within the deadline`);
      throw err;
    }
    const content = outcome.value || '';
    return { result: { content, total_chars: content.length, navigated_during: outcome.navigated_during }, page: await pageInfo(state) };
  },

  async links({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    const r = await withRetry(state, () => state.page.evaluate(linksFn, { filter: args.filter || '', max: args.max || 100 }));
    return { result: r.value, page: await pageInfo(state) };
  },

  async screenshot({ profile: name, target, args, deadline_ms }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    const type = args.format === 'png' ? 'png' : 'jpeg';
    const options = { path: args.path, type, fullPage: Boolean(args.full) };
    if (type === 'jpeg') options.quality = args.quality || 70;
    const scope = scopeLocator(state, args);
    const deadlineAt = Date.now() + Number(deadline_ms || 30000);
    let buffer;
    // The agent's picture is the page: the activity overlay steps aside for
    // both captures (the file and the inline copy).
    await state.overlay.suspend(true);
    let size; let inlinePath = null;
    try {
    try {
      // A background tab whose renderer stalled: select it quietly and try
      // once more, inside the op's deadline; an element capture is not retried.
      buffer = await captureWithQuietRetry({
        capture: (timeout) => (scope ? scope.screenshot({ ...options, timeout }) : state.page.screenshot({ ...options, timeout })),
        select: () => quietSelect(state),
        log: (text) => log('debug', text),
        deadlineAt,
        stallMs: SCREENSHOT_STALL_MS,
        scoped: Boolean(scope),
      });
    } catch (err) {
      if (err.code === 'tab_not_rendered') fail('tab_not_rendered', err.message);
      if (args.ref && /aria-ref|resolve/i.test(err.message)) fail('stale_ref', `ref ${args.ref} no longer resolves; take a new snapshot`);
      if (scope && /Timeout/i.test(err.message)) fail('element_not_visible', `${args.ref ? `ref ${args.ref}` : `selector ${args.selector}`} did not become visible in time; scroll it into view or take a new snapshot`);
      throw err;
    }
    size = imageSize(buffer, type);
    const maxPx = args.max_px || 1568;
    const longEdge = Math.max(size.width, size.height);
    if (longEdge > maxPx && args.inline_path) {
      // Downscale with CDP's clip.scale (no image library needed). Element shots
      // clip to the element's box in page coordinates (viewport box + scroll).
      const session = state.session;
      const scale = maxPx / longEdge;
      const metrics = await session.send('Page.getLayoutMetrics');
      const vis = metrics.cssVisualViewport || metrics.visualViewport;
      const content = metrics.cssContentSize || metrics.contentSize;
      let clip;
      if (scope) {
        const box = await scope.boundingBox();
        if (!box) fail('stale_ref', 'the element has no box any more; take a new snapshot');
        const scroll = await state.page.evaluate(() => ({ x: window.scrollX, y: window.scrollY }));
        clip = { x: box.x + scroll.x, y: box.y + scroll.y, width: box.width, height: box.height, scale };
      } else if (args.full) {
        clip = { x: 0, y: 0, width: content.width, height: content.height, scale };
      } else {
        clip = { x: vis.pageX, y: vis.pageY, width: vis.clientWidth, height: vis.clientHeight, scale };
      }
      const shot = await session.send('Page.captureScreenshot', { format: type, quality: type === 'jpeg' ? (args.quality || 70) : undefined, clip, captureBeyondViewport: Boolean(args.full || scope) });
      fs.writeFileSync(args.inline_path, Buffer.from(shot.data, 'base64'));
      inlinePath = args.inline_path;
    }
    } finally {
      await state.overlay.suspend(false);
    }
    return { result: { path: args.path, inline_path: inlinePath, width: size.width, height: size.height, bytes: buffer.length }, page: await pageInfo(state) };
  },

  async console({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    const since = Number(args.since || 0);
    const level = args.level || 'all';
    const keep = (e) => e.seq > since && (level === 'all' || (level === 'error' ? (e.level === 'error' || e.level === 'pageerror') : (e.level === 'warning' || e.level === 'warn' || e.level === 'error' || e.level === 'pageerror')));
    const entries = state.console.filter(keep).slice(-(args.max || 50));
    return { result: { entries, next_seq: state.consoleSeq }, page: await pageInfo(state) };
  },

  async network({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    const since = Number(args.since || 0);
    const match = (args.match || '').toLowerCase();
    const type = args.type || null;
    const entries = state.network.filter(e => e.seq > since && (!args.failed || e.failed) && (!match || e.url.toLowerCase().includes(match)) && (!type || e.type === type || (type === 'doc' && e.type === 'document'))).slice(-(args.max || 50));
    return { result: { entries, next_seq: state.networkSeq }, page: await pageInfo(state) };
  },

  async wait({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    const timeout = Number(args.timeout_ms || 30000);
    const started = Date.now();
    try {
      if (args.text) await state.page.waitForFunction((t) => document.body && document.body.innerText.includes(t), args.text, { timeout, polling: 250 });
      else if (args.gone) await state.page.waitForFunction((t) => !document.body || !document.body.innerText.includes(t), args.gone, { timeout, polling: 250 });
      else if (args.selector) await state.page.waitForSelector(args.selector, { timeout, state: 'visible' });
      else if (args.url) await state.page.waitForURL(args.url, { timeout, waitUntil: 'commit' });
      else if (args.load) await state.page.waitForLoadState(waitUntilOf(args.load), { timeout });
      else fail('invalid_request', 'nothing to wait for');
    } catch (err) {
      if (err instanceof OpError) throw err;
      if (/Timeout/i.test(err.message)) fail('browser_timeout', `condition not met within ${timeout} ms`);
      throw err;
    }
    return { result: { matched: true, elapsed_ms: Date.now() - started }, page: await pageInfo(state) };
  },

  async scroll({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    if (args.to && /^e\d+$/.test(args.to)) {
      try { await state.page.locator(`aria-ref=${args.to}`).scrollIntoViewIfNeeded({ timeout: 5000 }); }
      catch { fail('stale_ref', `ref ${args.to} no longer resolves; take a new snapshot`); }
    } else if (args.to === 'top') await state.page.evaluate(() => window.scrollTo(0, 0));
    else if (args.to === 'bottom') await state.page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
    else if (args.by !== undefined && args.by !== null) await state.page.evaluate((by) => window.scrollBy(0, by), Number(args.by));
    else fail('invalid_request', 'scroll needs --to top|bottom|eN or --by PX');
    await sleep(100);
    const pos = await state.page.evaluate(() => ({ scroll_y: Math.round(window.scrollY), scroll_height: document.documentElement.scrollHeight }));
    return { result: pos, page: await pageInfo(state) };
  },

  async eval({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    // The password rule: the page's password fields are snapshotted in the
    // isolated world (values stay in the page, reachable only through this
    // CDP session, never logged or returned); a change is undone and refused.
    // Fail closed: a guard that cannot run refuses the eval.
    const guard = args.guard_passwords ? await passwordSnapshot(state).catch(() => null) : false;
    if (guard === null) fail('password_field_refused', 'the password guard could not inspect the page; eval refused ([browser] type_into_password_fields = false)');
    const run = async () => {
      const value = await state.page.evaluate(args.expr);
      try { JSON.stringify(value); return value === undefined ? null : value; } catch { return String(value); }
    };
    let r;
    let navigated = false;
    try {
      if (guard) {
        // Never re-run under the guard: the snapshot belongs to the page that
        // was there; a navigation during the eval is a refusal, not a retry.
        try { r = { value: await run(), navigated_during: false }; }
        catch (err) { if (isNavigationError(err)) navigated = true; throw err; }
      } else {
        r = await withRetry(state, run);
      }
    } finally {
      if (guard) {
        if (navigated) fail('password_field_refused', 'the page navigated during a guarded eval; the result is discarded and the eval is not re-run ([browser] type_into_password_fields = false)');
        const changed = await passwordCheck(state, guard).catch(() => -1);
        if (changed !== 0) fail('password_field_refused', changed > 0 ? 'eval changed a password field; restored ([browser] type_into_password_fields = false)' : 'the password guard could not re-check the page after eval; refused');
      }
    }
    return { result: { value: r.value, navigated_during: r.navigated_during }, page: await pageInfo(state) };
  },

  async act({ profile: name, target, args, deadline_ms, activity }) {
    const state = needPage(needProfile(name), target);
    await guardDialog(state);
    const kind = args.kind;
    const page = state.page;
    const timeout = Math.max(1000, Math.min(10000, deadline_ms - 500));
    const hidden = await page.evaluate(() => document.visibilityState !== 'visible').catch(() => true);
    const scope = scopeLocator(state, args);
    if (!scope && kind !== 'press') fail('invalid_request', `${kind} needs a ref (from browser snapshot) or a selector`);
    let handle = null;
    if (scope) {
      try { handle = await scope.elementHandle({ timeout }); }
      catch (err) { if (args.ref) fail('stale_ref', `ref ${args.ref} no longer resolves (the page changed); re-run browser snapshot`); fail('invalid_request', `selector ${args.selector} matched nothing within the deadline`); }
      if (!handle) fail('stale_ref', `ref ${args.ref || args.selector} no longer resolves; re-run browser snapshot`);
    }
    // Names never carry a field's contents (no value, no innerText of an
    // editable); the element described is the one Playwright writes to (a
    // label's control, not the label); a frame counts as a password field
    // (what is inside it cannot be inspected).
    const describe = (el) => {
      const editable = (n) => Boolean(n) && (/^(input|textarea|select)$/i.test(n.tagName) || n.isContentEditable === true);
      if (!editable(el)) {
        const label = el.closest ? el.closest('label') : null;
        const control = el.control || (label && label.control);
        if (control) el = control;
      }
      const tag = el.tagName.toLowerCase();
      const role = el.getAttribute('role') || (tag === 'a' ? 'link' : tag === 'button' || (tag === 'input' && /^(button|submit|reset)$/.test(el.type)) ? 'button' : tag === 'select' ? 'combobox' : tag === 'textarea' || (tag === 'input') ? 'textbox' : tag);
      const name = (el.getAttribute('aria-label') || (el.labels && el.labels[0] && el.labels[0].innerText) || el.placeholder || (editable(el) ? '' : el.innerText) || el.getAttribute('title') || el.name || '').replace(/\s+/g, ' ').trim().slice(0, 60);
      const ac = (el.getAttribute('autocomplete') || '').toLowerCase();
      const password = (tag === 'input' && el.type === 'password') || ac === 'current-password' || ac === 'new-password' || tag === 'iframe' || tag === 'frame';
      return { role, name, password, tag };
    };
    const describeTarget = async () => {
      if (handle) return handle.evaluate(describe);
      // No target: the deep active element (through open shadow roots), as a
      // real handle; when it cannot be read the check fails closed.
      let active = null;
      try {
        active = await page.evaluateHandle(() => {
          let el = document.activeElement;
          while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
          return el && el !== document.body && el !== document.documentElement ? el : null;
        });
        const el = active.asElement();
        if (!el) return { role: 'page', name: '', password: false, tag: 'body' };
        return await el.evaluate(describe);
      } catch {
        return { role: 'page', name: '', password: true, tag: 'unknown' };
      } finally {
        if (active) await active.dispose().catch(() => {});
      }
    };
    const info = await describeTarget();
    const typing = kind === 'type' || kind === 'fill' || kind === 'press';
    if (typing && info.password && !args.allow_password) {
      fail('password_field_refused', `that is a password field (${info.role} "${info.name}"); ask the user to type it — \`herdr browser focus\` brings the window up ([browser] type_into_password_fields = false)`);
    }
    const urlBefore = page.url();
    // The activity cursor glides to the element first (the ripple lands with the click).
    if (activity && activity.frame && handle && !state.dialog) {
      const t0 = Date.now();
      try {
        await handle.scrollIntoViewIfNeeded({ timeout: 1000 }).catch(() => {});
        const box = await handle.boundingBox();
        if (box) {
          const x = Math.round(box.x + box.width / 2);
          const y = Math.round(box.y + box.height / 2);
          await state.overlay.moveTo(x, y, activity.animate !== false && !hidden);
          if (kind === 'click') state.overlay.ripple(x, y);
        }
      } catch (err) {
        log('debug', `overlay glide: ${err.message}`);
      }
      log('debug', `activity act:${kind} overlay ${Date.now() - t0} ms`);
    }
    state.inflight++;
    let navigated = false;
    let dispatched = false;
    const onNav = (frame) => { if (frame === page.mainFrame()) navigated = true; };
    page.on('framenavigated', onNav);
    try {
      const force = hidden;
      if (kind === 'click') await scope.click({ force, timeout });
      else if (kind === 'hover') {
        // A hidden tab has no pointer: the events are dispatched to the element
        // (CSS :hover does not change), and the result says so.
        if (hidden) { dispatched = true; await handle.evaluate((el) => { for (const type of ['pointerover', 'mouseover', 'mouseenter']) el.dispatchEvent(new MouseEvent(type, { bubbles: type !== 'mouseenter' })); }); }
        else await scope.hover({ timeout });
      }
      else if (kind === 'fill') await scope.fill(String(args.text ?? ''), { force, timeout });
      else if (kind === 'select') await scope.selectOption(String(args.value ?? ''), { force, timeout }).catch(async (err) => { if (/did not find some options|not found/i.test(err.message)) await scope.selectOption({ label: String(args.value ?? '') }, { force, timeout }); else throw err; });
      else if (kind === 'type') {
        if (args.clear) await scope.fill('', { force, timeout });
        await scope.focus({ timeout });
        await scope.pressSequentially(String(args.text ?? ''), { timeout });
        if (args.submit) await scope.press('Enter', { timeout });
      }
      else if (kind === 'press') {
        if (scope) await scope.press(String(args.key), { timeout }); else await page.keyboard.press(String(args.key));
      }
      else fail('invalid_request', `unknown act ${kind}`);
      // A step that navigated: the next one runs after domcontentloaded.
      await sleep(120);
      if (navigated || page.url() !== urlBefore) await page.waitForLoadState('domcontentloaded', { timeout: Math.max(1000, timeout) }).catch(() => {});
    } catch (err) {
      if (err instanceof OpError) throw err;
      if (/Timeout/i.test(err.message)) fail(args.ref ? 'stale_ref' : 'browser_timeout', `${kind} did not complete within ${timeout} ms${args.ref ? ' (the ref may be stale; re-run browser snapshot)' : ''}`);
      throw err;
    } finally {
      page.off('framenavigated', onNav);
      state.inflight--;
    }
    return { result: { kind, role: info.role, name: info.name, navigated: navigated || page.url() !== urlBefore, url_before: urlBefore, dispatched }, page: await pageInfo(state) };
  },

  // The pinned dashboard follows [browser] pin_dashboard live.
  async dashboard({ args }) {
    let applied = 0;
    for (const profile of profiles.values()) {
      if (profile.companion.state !== 'ready') continue;
      try { await profile.companion.dashboard(Boolean(args.pin)); applied++; }
      catch (err) { log('debug', `dashboard: ${err.message}`); }
    }
    return { applied };
  },

  // The new tab page's snapshot (herdr pushes it debounced): into every
  // ready companion's session storage; best effort.
  async ntp({ args }) {
    let pushed = 0;
    for (const profile of profiles.values()) {
      if (profile.companion.state !== 'ready') continue;
      try {
        // target -> Chrome tab id, so a click on the page activates exactly
        // that tab (resolved once per page, cached by the companion driver).
        const tabIds = {};
        for (const state of profile.pages.values()) {
          const id = await profile.companion.tabIdFor(state, true).catch(() => null);
          if (id != null) tabIds[state.target] = id;
        }
        const snapshot = args.snapshot ? { ...args.snapshot, tabIds } : null;
        await profile.companion.call('herdrSnapshot', snapshot);
        pushed++;
      } catch (err) { log('debug', `ntp push: ${err.message}`); }
    }
    return { pushed };
  },

  // Panes that are gone: the overlays on their pages go now (whatever the
  // companion's state) and their tab groups dissolve, in every attached
  // profile. Keys whose group could not be released (no companion, an
  // error) come back as `failed`, for herdr to retry later.
  async release({ args }) {
    const keys = Array.isArray(args.keys) ? args.keys.filter((k) => typeof k === 'string' && k).map(String) : [];
    const dismissed = dismissPanes(profiles.values(), keys);
    if (dismissed) log('debug', `release: ${dismissed} overlay(s) dismissed`);
    const ready = [...profiles.values()].filter((profile) => profile.companion.state === 'ready');
    if (!ready.length) return { released: [], failed: keys, reason: 'companion not ready' };
    const failed = new Set();
    for (const profile of ready) {
      for (const key of keys) {
        try { await profile.companion.release([key]); }
        catch (err) { failed.add(key); log('debug', `release ${key}: ${err.message}`); }
      }
    }
    return { released: keys.filter((k) => !failed.has(k)), failed: [...failed] };
  },

  async dialog({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    if (!(await dialogStillOpen(state))) fail('no_dialog', 'no dialog is open on this tab');
    const dialog = state.dialog;
    state.dialog = null;
    try { if (args.accept) await dialog.accept(args.text || undefined); else await dialog.dismiss(); }
    catch (err) { fail('no_dialog', `the dialog was already closed (${String(err.message).split('\n')[0]})`); }
    event('dialog', { profile: name, target, state: 'closed' });
    return { result: { type: dialog.type(), message: dialog.message() }, page: await pageInfo(state) };
  },
};

// The password guard around eval: every password field (input[type=password],
// autocomplete current-/new-password; through open shadow roots and
// same-origin frames) with its value and type, as an object in the isolated
// world held by a remote object id. Nothing crosses to the sidecar but the id.
const PASSWORD_SNAPSHOT_JS = `(() => {
  const out = [];
  const seen = new Set();
  const walk = (root) => {
    if (!root || seen.has(root)) return;
    seen.add(root);
    for (const el of root.querySelectorAll('input')) {
      const ac = (el.getAttribute('autocomplete') || '').toLowerCase();
      if (el.type === 'password' || ac === 'current-password' || ac === 'new-password') out.push({ el, value: el.value, type: el.type });
    }
    for (const host of root.querySelectorAll('*')) if (host.shadowRoot) walk(host.shadowRoot);
    for (const frame of root.querySelectorAll('iframe,frame')) { try { walk(frame.contentDocument); } catch {} }
  };
  walk(document);
  return out;
})()`;
async function passwordSnapshot(state) {
  const contextId = await state.overlay.context();
  const r = await state.session.send('Runtime.evaluate', { expression: PASSWORD_SNAPSHOT_JS, contextId, returnByValue: false });
  if (r.exceptionDetails || !r.result || !r.result.objectId) throw new Error('password snapshot failed');
  return r.result.objectId;
}
// Fields whose value changed are restored (type too when it moved away from
// password); returns how many were restored, throws when the check cannot run.
async function passwordCheck(state, objectId) {
  try {
    const r = await state.session.send('Runtime.callFunctionOn', {
      objectId,
      functionDeclaration: `function () {
        let restored = 0;
        for (const rec of this) {
          const el = rec.el;
          if (!el || !el.isConnected) continue;
          if (el.type !== rec.type) { try { el.type = rec.type; } catch {} }
          if (el.value !== rec.value) { try { el.value = rec.value; } catch {} restored++; }
        }
        return restored;
      }`,
      returnByValue: true,
    });
    if (r.exceptionDetails) throw new Error('password check failed');
    return Number(r.result && r.result.value) || 0;
  } finally {
    state.session.send('Runtime.releaseObject', { objectId }).catch(() => {});
  }
}

// Keep only actionable lines (and headings for orientation) of an aria snapshot.
function interactiveOnly(snapshot) {
  const keep = /^\s*- (button|link|textbox|checkbox|radio|combobox|listbox|option|menuitem|menuitemcheckbox|menuitemradio|slider|spinbutton|switch|tab|searchbox|heading|dialog|alertdialog)\b/;
  return snapshot.split('\n').filter(line => keep.test(line)).join('\n');
}

// PNG / JPEG dimensions from the header.
function imageSize(buffer, type) {
  try {
    if (type === 'png') return { width: buffer.readUInt32BE(16), height: buffer.readUInt32BE(20) };
    let i = 2;
    while (i < buffer.length) {
      if (buffer[i] !== 0xff) { i++; continue; }
      const marker = buffer[i + 1];
      if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
        return { height: buffer.readUInt16BE(i + 5), width: buffer.readUInt16BE(i + 7) };
      }
      i += 2 + buffer.readUInt16BE(i + 2);
    }
  } catch {}
  return { width: 0, height: 0 };
}

// ---------------------------------------------------------------------------
// Main loop

// Ops that work on a page: the activity frame pulses while they run and stays, calmer, for the directive's window after.
const PAGE_OPS = new Set(['navigate', 'history', 'read', 'links', 'screenshot', 'console', 'network', 'wait', 'scroll', 'eval', 'act', 'dialog', 'focus', 'select']);
function activityPage(name, target) {
  const profile = profiles.get(name);
  const state = profile && target ? profile.pages.get(target) : null;
  return state && !state.closed && !state.dialog ? state : null;
}
async function activityBegin(state, activity) {
  // the pane this page belongs to, for `release` (independent of the companion's groups)
  if (activity.group && activity.group.key) state.paneKey = String(activity.group.key);
  if (!activity.frame) return;
  await state.overlay.show(activity.color).catch((err) => log('debug', `overlay: ${err.message}`));
}
function activityEnd(state, activity) {
  if (activity.frame) state.overlay.linger(activity.linger_ms);
  if (activity.group && state.profile.companion.state === 'ready') state.profile.companion.touch(state, activity.group);
}

async function handle(req) {
  const op = ops[req.op];
  if (!op) return send({ id: req.id, ok: false, error: { code: 'unknown_op', message: `unknown op ${req.op}; run herdr browser setup` } });
  const deadline = Number(req.deadline_ms || 30000);
  const activity = req.activity && typeof req.activity === 'object' ? req.activity : null;
  let state = activity && PAGE_OPS.has(req.op) ? activityPage(req.profile, req.target) : null;
  if (state) await activityBegin(state, activity);
  try {
    const out = await withTimeout(op({ profile: req.profile, target: req.target, args: req.args || {}, deadline_ms: deadline, activity }), deadline, 'browser_timeout', `${req.op} exceeded ${deadline} ms`);
    if (activity && req.op === 'open' && out && out.result && out.result.target) {
      // The tab an agent just opened: frame up, grouped.
      state = activityPage(req.profile, out.result.target);
      if (state) await activityBegin(state, activity);
    }
    if (out && typeof out === 'object' && 'result' in out && 'page' in out) send({ id: req.id, ok: true, result: out.result, page: out.page });
    else send({ id: req.id, ok: true, result: out || {} });
  } catch (err) {
    const code = err instanceof OpError ? err.code : (/Target closed|has been closed/i.test(err.message) ? 'tab_closed' : 'browser_error');
    const error = { code, message: String(err && err.message || err).split('\n')[0].slice(0, 500) };
    if (err && err.target) error.target = String(err.target);
    send({ id: req.id, ok: false, error });
  } finally {
    if (state && !state.closed) activityEnd(state, activity);
  }
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on('line', (line) => {
  if (!line.trim()) return;
  let req;
  try { req = JSON.parse(line); } catch { return log('warn', 'bad request line'); }
  handle(req);
});
rl.on('close', () => process.exit(0));
process.stdin.on('end', () => process.exit(0));
process.on('uncaughtException', (err) => log('error', `uncaught: ${err && err.stack || err}`));
process.on('unhandledRejection', (err) => log('error', `unhandled: ${err && err.stack || err}`));
