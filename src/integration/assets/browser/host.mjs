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
import { Overlay, Companion, NUDGE_URL, dismissPanes, captureWithQuietRetry, CaptureLane, laneShot, shotParams, firstShot, cleanCapture, clearClipEmulation, SELECT_BUDGET_MS, describe, probePages, ControlPage, ViewWatches, isFurnitureUrl, VIEW_MARKER, CompanionDialog, reconcileDialogs, pruneAnswered, DIALOG_BINDING } from './activity.mjs';
import { pathToFileURL } from 'node:url';
import nodePath from 'node:path';
// the act's change probe and timeout message (on a line of their own)
import { MODALS_SOURCE, actChanges, actTimeoutMessage, redactActText } from './extract.mjs';
/** Kept out of the connect for the post-timeout diagnosis (the page probe: a 1 s list fetch, 1 s probes). */
const ATTACH_DIAG_RESERVE_MS = 2500;

const require = createRequire(import.meta.url);
// Real functions for page.evaluate: a string would be evaluated as an expression (yielding the function,
// not its result) and an in-page eval would hit strict CSPs.
const extractFn = new Function('args', 'return (' + EXTRACT_SOURCE + ')(args);');
const linksFn = new Function('args', 'return (' + LINKS_SOURCE + ')(args);');
const modalsFn = new Function('acted', 'return (' + MODALS_SOURCE + ')(acted);');
const PLAYWRIGHT_VERSION = require('playwright-core/package.json').version;
const HOST_PROTOCOL = 1;
const CONSOLE_RING = 1000;
const NETWORK_RING = 500;
const TEXT_CAP = 2048;
const HTML_CAP = 200 * 1024;
const SCREENSHOT_STALL_MS = 5000;

const profiles = new Map(); // name -> Profile
// Fork (companion browser): file watches of HTML views (`show`, `watch`).
const watches = new ViewWatches({ watchFn: (path, cb) => fs.watch(path, cb), log: (text) => log('debug', text) });
/** An error message for a tab event: one line, capped. */
const eventText = (text) => String(text == null ? '' : text).split('\n')[0].slice(0, 300);

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}
function event(name, fields) {
  send(Object.assign({ event: name }, fields));
}
function log(level, text) {
  event('log', { level, text: String(text).slice(0, 500) });
}
// An act that timed out: the whole Playwright message (its call log, a
// submitted text masked) goes to host.log (the sidecar's stderr), a summary to
// herdr's log at info; the URL comes without its query or fragment.
function logActTimeout(fields) {
  try { process.stderr.write(`${new Date().toISOString()} act timeout ${JSON.stringify(fields)}\n`); } catch {}
  const { message, ...rest } = fields;
  log('info', `act timeout ${JSON.stringify(rest)} ${String(message || '').split('\n')[0]}`);
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
    this.captures = new CaptureLane(); // every Page.captureScreenshot of this page, one at a time
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
    // Fork (companion browser): `companion` for a registered browser host
    // (driven through its control page), else a herdr-launched Chromium.
    this.kind = 'chromium';
    this.control = null;
    this.hostDetail = '';
    // Fork (companion dialogs): the companion's last pushed dialogs, target -> dialog
    this.dialogSeq = 0;
    this.dialogs = new Map();
    this.dialogsAnswered = new Set();
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
  // the page's own context (a companion's views may live in several)
  const session = await (page.context ? page.context() : profile.ctx).newCDPSession(page);
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
  if (isFurnitureUrl(page.url())) return null; // the companion browser's own pages (control page, strip)
  if (profile.pending.has(page)) return profile.pending.get(page);
  const promise = (async () => {
    const { targetId, session } = await targetIdOf(profile, page);
    const state = new PageState(profile, page, targetId);
    profile.pages.set(targetId, state);
    state.session = session;
    if (profile.kind === 'companion') syncCompanionDialogs(profile); // one it pushed before the page was tracked
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
      if (level === 'error') event('tab', { profile: profile.name, target: targetId, kind: 'console_error', text: eventText(msg.text()) });
    });
    page.on('pageerror', (err) => {
      state.pushConsole({ ts: Date.now(), level: 'pageerror', text: String(err && err.message || err).slice(0, TEXT_CAP), url: '', line: undefined });
      event('tab', { profile: profile.name, target: targetId, kind: 'page_error', text: eventText(err && err.message || err) });
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
      // a main-frame load that failed (not one the user or a new navigation stopped, nor our
      // own network block of an html view); a
      // companion reports its own views' failed loads and crashes (one source, one message)
      if (profile.kind !== 'companion' && req.isNavigationRequest() && req.frame() === page.mainFrame() && failure && !/ERR_ABORTED|ERR_BLOCKED_BY_CLIENT/.test(failure.errorText)) {
        event('tab', { profile: profile.name, target: targetId, kind: 'load_failed', text: eventText(`${failure.errorText} ${req.url()}`) });
      }
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
      watches.disarm(targetId);
      if (profile.control) profile.control.forget(targetId);
      state.overlay.dispose();
      profile.companion.forget(targetId);
      profile.pages.delete(targetId);
      profile.pending.delete(page);
      event('tab', { profile: profile.name, target: targetId, kind: 'closed' });
    });
    page.on('crash', () => { if (profile.kind !== 'companion') event('tab', { profile: profile.name, target: targetId, kind: 'crashed' }); });
    page.on('response', (res) => {
      if (!res.request().isNavigationRequest() || res.frame() !== page.mainFrame()) return;
      state.lastStatus = res.status();
      if (res.status() >= 500) event('tab', { profile: profile.name, target: targetId, kind: 'load_failed', text: eventText(`HTTP ${res.status()} ${res.url()}`) });
    });
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
    return { ...await tabs(profile), companion: profile.companion.info() };
  }
  profile.port = port;
  const t0 = Date.now();
  const stamp = () => `attach ${profile.name}:${port}`;
  // Restored tabs Chrome never loaded hang the connect (Playwright awaits
  // every page's init): the companion reloads them and they settle first,
  // inside the first half of the deadline. A failed sweep is a result, not
  // an error: the connect runs as before and the text names the state.
  const deadlineAt = t0 + deadlineMs;
  const swept = await profile.companion.sweep(Math.min(8000, Math.floor(deadlineMs / 2)));
  log('debug', `${stamp()} sweep ${JSON.stringify(swept)}`);
  // An activity-off profile never keeps pinned dashboards: the worker's own
  // default is pinned, so it is told before the connect (a blocked connect
  // would otherwise leave them).
  let unpinned = false;
  if (pinDashboard === false && swept.state === 'ready') {
    unpinned = await withTimeout(profile.companion.call('herdrDashboard', { pin: false }), Math.min(2000, Math.max(1, deadlineAt - Date.now() - ATTACH_DIAG_RESERVE_MS - 3000)), 'x', 'x').then(() => true).catch((err) => { log('debug', `${stamp()} dashboard off: ${err.message}`); return false; });
  }
  // (never 0: Playwright reads a 0 timeout as none)
  const connectMs = Math.max(1, deadlineAt - Date.now() - ATTACH_DIAG_RESERVE_MS);
  log('debug', `${stamp()} connectOverCDP… (${connectMs} ms)`);
  let browser;
  try {
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true, timeout: connectMs });
  } catch (err) {
    const message = String(err && err.message || err).split('\n')[0];
    log('debug', `${stamp()} connectOverCDP failed after ${Date.now() - t0} ms: ${message}`);
    // A distinct code for the connect timeout: herdr retries that once. The
    // text says what blocks: the page targets that do not answer DevTools (a
    // dialog, a frozen page), the tabs that could not be loaded, what is
    // still loading — never a stop (a restart restores the same tabs).
    if (/Timeout/i.test(message)) {
      const probe = await probePages(port, 1000, null, undefined, deadlineAt - 300).catch(() => ({ pages: 0, blocked: [] }));
      log('debug', `${stamp()} probe ${JSON.stringify(probe)}`);
      fail('attach_timeout', `the browser did not accept the CDP connection within ${deadlineMs} ms: ${describe(swept, probe.blocked)}`);
    }
    throw err;
  }
  log('debug', `${stamp()} connected in ${Date.now() - t0} ms, pages ${browser.contexts()[0] ? browser.contexts()[0].pages().length : 'none'}`);
  profile.browser = browser;
  const ctx = browser.contexts()[0];
  if (!ctx) fail('browser_start_failed', 'the browser has no default context');
  profile.ctx = ctx;
  wireContext(profile, ctx);
  wireBrowser(profile, browser);
  await Promise.all(ctx.pages().map(page => track(profile, page, null).catch((err) => log('warn', `track failed: ${err.message}`))));
  log('debug', `${stamp()} tracked in ${Date.now() - t0} ms`);
  event('browser', { profile: profile.name, kind: 'attached', detail: browser.version() });
  // The companion extension: what the sweep found (it adopted the worker),
  // else a probe bounded by what is left of the deadline.
  const leftMs = () => Math.max(1, deadlineAt - Date.now());
  const companion = swept.state === 'ready'
    ? profile.companion.info()
    : await withTimeout(profile.companion.probe(), Math.min(6000, leftMs()), 'x', 'x').catch(() => ({ state: 'missing', detail: 'probe timed out' }));
  log('debug', `${stamp()} companion ${companion.state}${companion.detail ? ' (' + companion.detail + ')' : ''}`);
  // the dashboard: not again when the pre-connect un-pin took; about 1.2 s is kept for tabs()
  const dashboardMs = Math.min(3000, leftMs() - 1200);
  if (companion.state === 'ready' && pinDashboard !== undefined && !(pinDashboard === false && unpinned) && dashboardMs > 0) {
    await withTimeout(profile.companion.dashboard(pinDashboard), dashboardMs, 'x', 'x').catch((err) => log('debug', `dashboard: ${err.message}`));
  }
  return Object.assign(await tabs(profile), { companion });
}

// No-op recording dialog listener: without any listener Playwright auto-dismisses
// every dialog, including the user's. With it the dialog stays open for the user.
function wireContext(profile, ctx) {
  ctx.on('dialog', async (dialog) => {
    const page = dialog.page();
    let state = null;
    for (const s of profile.pages.values()) if (s.page === page) state = s;
    if (!state) { try { state = await track(profile, page, null); } catch { return; } }
    if (!state) return;
    state.dialog = dialog;
    event('dialog', { profile: profile.name, target: state.target, type: dialog.type(), message: dialog.message().slice(0, 500), state: 'open' });
  });
  ctx.on('page', (page) => {
    track(profile, page, 'other').catch((err) => log('warn', `track failed: ${err.message}`));
  });
}
function wireBrowser(profile, browser) {
  browser.on('disconnected', () => {
    profile.browser = null;
    profile.ctx = null;
    profile.companion.close();
    for (const [target] of profile.pages) { watches.disarm(target); profile.pages.delete(target); }
    // A detach herdr asked for is not a lost browser (it may already hold a new registration).
    if (profiles.get(profile.name) !== profile) return;
    event('browser', { profile: profile.name, kind: 'disconnected', detail: 'CDP connection closed' });
  });
}

// ---------------------------------------------------------------------------
// Fork (companion browser): a registered browser host. No extension sweep,
// no dashboard, no new tab page: the control page answers the nonce, then
// every page but the companion's own is tracked.

const allPages = (profile) => (profile.browser ? profile.browser.contexts().flatMap((ctx) => ctx.pages()) : []);

async function attachCompanion(profile, port, deadlineMs, nonce) {
  profile.kind = 'companion';
  profile.companion.state = 'host'; // never 'ready': the extension paths stay off
  if (profile.browser && profile.browser.isConnected() && profile.port === port) {
    await profile.control.verify(nonce);
    return Object.assign(await tabs(profile), { companion: { state: 'host', detail: profile.hostDetail } });
  }
  if (profile.browser) { await profile.browser.close().catch(() => {}); profile.browser = null; }
  profile.port = port;
  const t0 = Date.now();
  let browser;
  try {
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true, timeout: Math.max(1, deadlineMs - 1000) });
  } catch (err) {
    const message = String(err && err.message || err).split('\n')[0];
    fail(/Timeout/i.test(message) ? 'attach_timeout' : 'host_unreachable', `the companion did not accept the CDP connection: ${message}`);
  }
  const ctx = browser.contexts()[0];
  if (!ctx) { await browser.close().catch(() => {}); fail('browser_start_failed', 'the companion has no browser context'); }
  profile.browser = browser;
  profile.ctx = ctx;
  profile.control = new ControlPage({ pages: () => allPages(profile), evaluate: (page, fn, arg) => page.evaluate(fn, arg) });
  try {
    await profile.control.verify(nonce);
  } catch (err) {
    profile.browser = null; profile.ctx = null; profile.control = null;
    await browser.close().catch(() => {});
    fail(err.code || 'host_verify_failed', err.message);
  }
  for (const context of browser.contexts()) wireContext(profile, context);
  wireBrowser(profile, browser);
  await trackAll(profile);
  await watchCompanionDialogs(profile);
  profile.hostDetail = `electron ${browser.version()}`;
  log('debug', `attach ${profile.name}:${port} (companion) in ${Date.now() - t0} ms`);
  event('browser', { profile: profile.name, kind: 'attached', detail: browser.version() });
  return Object.assign(await tabs(profile), { companion: { state: 'host', detail: profile.hostDetail } });
}

/** Fork (companion dialogs): pages' alert / confirm / prompt live in the
 *  companion (no CDP dialog events). Its pushes reach a binding on the
 *  control page; the current list is read once at attach (one opened before,
 *  or across a herdr restart). A companion without dialogs() just never
 *  pushes. */
async function watchCompanionDialogs(profile) {
  profile.dialogSeq = 0;
  profile.dialogs = new Map();
  profile.dialogsAnswered = new Set();
  const page = profile.control && profile.control.find();
  if (!page) return;
  try {
    await page.exposeBinding(DIALOG_BINDING, (_source, payload) => applyCompanionDialogs(profile, payload));
  } catch (err) { log('debug', `dialog binding: ${String(err && err.message).split('\n')[0]}`); }
  const now = await withTimeout(profile.control.call('dialogs'), 2000, 'x', 'x').catch(() => null);
  if (now) applyCompanionDialogs(profile, now);
}
function applyCompanionDialogs(profile, payload) {
  if (!payload || typeof payload !== 'object' || !Array.isArray(payload.dialogs) || profile.kind !== 'companion') return;
  const seq = Number(payload.seq) || 0;
  if (seq < profile.dialogSeq) return; // an older answer (the attach read) after a newer push
  profile.dialogSeq = seq;
  pruneAnswered(profile.dialogsAnswered, payload.dialogs);
  profile.dialogs = new Map(payload.dialogs.filter((d) => d && d.target_id).map((d) => [String(d.target_id), d]));
  syncCompanionDialogs(profile);
}
function syncCompanionDialogs(profile) {
  if (!profile.control) return;
  const current = new Map();
  for (const [target, state] of profile.pages) {
    if (state.closed) continue;
    current.set(target, state.dialog && state.dialog.companion ? state.dialog : null);
  }
  const { open, close } = reconcileDialogs(current, [...profile.dialogs.values()], profile.dialogsAnswered);
  for (const [target, d] of open) {
    const state = profile.pages.get(target);
    state.dialog = new CompanionDialog(profile.control, d, (id) => profile.dialogsAnswered.add(id), (id) => profile.dialogsAnswered.delete(id));
    event('dialog', { profile: profile.name, target, type: state.dialog.type(), message: state.dialog.message().slice(0, 500), state: 'open' });
  }
  for (const target of close) {
    const state = profile.pages.get(target);
    state.dialog = null;
    event('dialog', { profile: profile.name, target, state: 'closed' });
  }
}

/** Track every page of every context not tracked yet (a view another context created). */
async function trackAll(profile) {
  await Promise.all(allPages(profile).map((page) => track(profile, page, null).catch((err) => log('warn', `track failed: ${err.message}`))));
}

/** The target id of the companion view: as answered, else its marker URL / file URL. */
async function viewTarget(profile, answer, matchUrl, deadlineAt) {
  if (answer && answer.target_id) return String(answer.target_id);
  while (Date.now() < deadlineAt) {
    for (const page of allPages(profile)) {
      if (matchUrl(page.url())) {
        const { targetId, session } = await targetIdOf(profile, page);
        await session.detach().catch(() => {});
        return targetId;
      }
    }
    await sleep(50);
  }
  fail('browser_start_failed', 'the companion created the view but it never showed up');
}

/** Wait until a target is tracked (a view of another context: track it ourselves). */
async function waitTracked(profile, targetId, deadlineAt) {
  let state = profile.pages.get(targetId);
  let swept = false;
  while (!state && Date.now() < deadlineAt) {
    await sleep(50);
    state = profile.pages.get(targetId);
    if (!state && !swept && profile.kind === 'companion') { swept = true; await trackAll(profile); state = profile.pages.get(targetId); }
  }
  if (!state) fail('browser_start_failed', 'the new tab did not show up');
  return state;
}

/** HTML views without network: http(s) aborted, WebSockets never reach a server. */
async function blockNetwork(state) {
  await state.page.route((url) => /^(https?|wss?):/i.test(String(url && url.href || url)), (route) => route.abort('blockedbyclient'));
  if (typeof state.page.routeWebSocket === 'function') await state.page.routeWebSocket(/.*/, (ws) => ws.close()).catch(() => {});
}

async function reloadView(name, target) {
  const profile = profiles.get(name);
  const state = profile && profile.pages.get(target);
  if (!state || state.closed) { watches.disarm(target); return; }
  await state.page.reload({ waitUntil: 'domcontentloaded' });
}

async function tabs(profile) {
  // every tab asked at once: the per-tab caps no longer add up over many tabs
  const states = [...profile.pages.values()].filter((state) => !state.closed && !isFurnitureUrl(state.page.url()));
  const out = await Promise.all(states.map(async (state) => {
    let selected = false;
    let title = '';
    if (!state.dialog) {
      selected = await withTimeout(state.page.evaluate(() => document.visibilityState === 'visible'), 500, 'x', 'x').catch(() => false);
      title = await withTimeout(state.page.title(), 500, 'x', 'x').catch(() => '');
    }
    return { target: state.target, url: state.page.url(), title, selected, dialog_open: Boolean(state.dialog) };
  }));
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
async function companionDialogStillOpen(state) {
  const profile = state.profile;
  const now = profile.control ? await withTimeout(profile.control.call('dialogs'), 500, 'x', 'x').catch(() => null) : null;
  if (now) applyCompanionDialogs(profile, now);
  return Boolean(state.dialog);
}
// A dialog the user answered in the window: the renderer answers again, so a quick evaluate
// resolving means the dialog is gone (belt and braces next to Page.javascriptDialogClosed).
async function dialogStillOpen(state) {
  if (!state.dialog) return false;
  // the companion pushes its close (a frame's dialog leaves the page answering); a quick
  // re-read of its list in case a push was lost
  if (state.dialog.companion) return companionDialogStillOpen(state);
  const gone = await withTimeout(state.page.evaluate('1').then(() => true), 300, 'x', 'x').catch(() => false);
  if (gone) {
    state.dialog = null;
    event('dialog', { profile: state.profile.name, target: state.target, state: 'closed' });
  }
  return Boolean(state.dialog);
}
/** The companion's quiet tab selection for a page: true when it happened (never raises the window). */
async function quietSelect(state) {
  if (state.profile.kind === 'companion') {
    return withTimeout(controlCall(state.profile, 'select', state.target), SELECT_BUDGET_MS, 'x', 'x').then(() => true).catch((err) => { log('debug', `quiet select: ${err.message}`); return false; });
  }
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

/** A control page call for a target's view (companion profiles). */
async function controlCall(profile, fn, target) {
  const view = await profile.control.viewOf(target);
  return profile.control.call(fn, view);
}

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
    if (args.kind === 'companion') return attachCompanion(profile, args.port, deadline_ms, String(args.nonce || ''));
    return attach(profile, args.port, deadline_ms, args.pin_dashboard);
  },
  async detach({ profile: name }) {
    const profile = profiles.get(name);
    if (profile) for (const target of profile.pages.keys()) watches.disarm(target);
    profiles.delete(name); // first: its close is no `disconnected` event
    if (profile && profile.browser) await profile.browser.close().catch(() => {});
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
      if (profile.kind === 'companion') {
        // the companion creates the view in the caller's group (else the shown one)
        const answer = await profile.control.call('open', { tab_id: args.tab_id || null, url: 'about:blank', background: args.background !== false, opened_by: args.opened_by || undefined });
        const viewId = answer && answer.view_id ? String(answer.view_id) : '';
        const id = await viewTarget(profile, answer, (url) => viewId !== '' && url === VIEW_MARKER + viewId, Date.now() + Math.min(deadline_ms, 10000));
        profile.control.remember(id, viewId);
        return id;
      }
      const id = await createTarget(profile, 'about:blank', args.background !== false);
      if (activity && activity.group && profile.companion.state === 'ready') {
        await withTimeout(profile.companion.adoptNew(id), 1500, 'x', 'x').catch((err) => log('debug', `companion adopt: ${err.message}`));
      }
      return id;
    });
    const state = await waitTracked(profile, targetId, Date.now() + Math.min(deadline_ms, 10000));
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
    const profile = needProfile(name);
    const state = needPage(profile, target);
    // a companion view is destroyed by the companion (its strip follows)
    if (profile.kind === 'companion') { await controlCall(profile, 'close', target); return {}; }
    await state.page.close({ runBeforeUnload: false });
    return {};
  },

  // The one op that raises the window: the login handoff to the user
  // (`browser focus`). Everything else selects tabs quietly (`select`).
  async focus({ profile: name, target, args }) {
    const profile = needProfile(name);
    const state = needPage(profile, target);
    // companion: the user's focus takes the keyboard, an agent's only orders the window front
    // (an older companion reads only the bare view id: ControlPage.focusView)
    if (profile.kind === 'companion') {
      const view = await profile.control.viewOf(target);
      await profile.control.focusView(view, Boolean(args && args.user));
    } else await state.page.bringToFront();
    return { result: {}, page: await pageInfo(state) };
  },

  // Fork (companion launcher): the managed companion quits through its control page
  // (herdr's stop, before SIGTERM); and raises its window (`user`: with the keyboard).
  async control_quit({ profile: name }) {
    const profile = needProfile(name);
    if (profile.kind !== 'companion') fail('invalid_request', 'control_quit is for the companion host');
    await profile.control.call('quit');
    return {};
  },
  async control_raise({ profile: name, args }) {
    const profile = needProfile(name);
    if (profile.kind !== 'companion') fail('invalid_request', 'control_raise is for the companion host');
    await profile.control.raise(Boolean(args && args.user));
    return {};
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
      // the main content when it has some (an open dialog comes first in both)
      return (args.selector ? r.markdown : (r.markdown && r.main_chars >= 200 ? r.markdown : r.full_markdown));
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
    const quality = args.quality || 70;
    const scope = scopeLocator(state, args);
    const deadlineAt = Date.now() + Number(deadline_ms || 30000);
    const left = () => Math.max(1000, deadlineAt - Date.now());
    // Raw CDP on the host's own session, through the page's capture lane:
    // Playwright's page.screenshot abandons a capture that timed out while
    // Chromium still holds it, and a second one sent beside it (the quiet
    // retry, the inline copy, another pane's shot) left the tab stretched to
    // its document's height. A viewport shot sends no clip at all.
    // Past the op's deadline (the caller already has its browser_timeout) no
    // capture is sent: the lane checks it when the request's turn comes.
    // A clipped capture waits for a tab with a size and its document loaded,
    // and the device metrics it sets are always taken back (firstShot,
    // clipReady in activity.mjs).
    const shots = [];
    let buffer;
    // The agent's picture is the page: the activity overlay steps aside for
    // both captures (the file and the inline copy).
    await state.overlay.suspend(true);
    let size; let inlinePath = null;
    try {
    // An element's box in page coordinates, scrolled into view first (visible
    // is enough, as for Playwright's element shot: waiting for it to be stable
    // needs animation frames a background tab does not run).
    const elementBox = async () => {
      const timeout = Math.min(SCREENSHOT_STALL_MS, left());
      await scope.waitFor({ state: 'visible', timeout });
      await scope.evaluate((el) => el.scrollIntoView({ block: 'nearest', inline: 'nearest' }), undefined, { timeout });
      const box = await scope.boundingBox({ timeout });
      if (!box || !box.width || !box.height) {
        if (args.ref) fail('stale_ref', `ref ${args.ref} has no box any more; take a new snapshot`);
        fail('element_not_visible', `selector ${args.selector} is not visible; scroll it into view or take a new snapshot`);
      }
      const scroll = await state.page.evaluate(() => ({ x: window.scrollX, y: window.scrollY }));
      return { x: box.x + scroll.x, y: box.y + scroll.y, width: box.width, height: box.height };
    };
    let element = null;
    try {
      const answer = await firstShot({
        send: (method, params) => state.session.send(method, params),
        lane: state.captures,
        type,
        quality,
        element: scope ? async () => (element = await elementBox()) : null,
        full: Boolean(args.full),
        select: () => quietSelect(state),
        log: (text) => log('debug', text),
        deadlineAt,
        stallMs: SCREENSHOT_STALL_MS,
        shots,
      });
      buffer = Buffer.from(answer.data, 'base64');
    } catch (err) {
      if (err instanceof OpError) throw err;
      if (err.code === 'tab_not_rendered') fail('tab_not_rendered', err.message);
      if (args.ref && /aria-ref|resolve/i.test(err.message)) fail('stale_ref', `ref ${args.ref} no longer resolves; take a new snapshot`);
      // The element's box was measured, so a timeout now is the capture's:
      // the tab did not draw, not an invisible element.
      if (scope && element && /Timeout/i.test(err.message)) fail('tab_not_rendered', 'the tab could not be drawn; try again');
      if (scope && /Timeout/i.test(err.message)) fail('element_not_visible', `${args.ref ? `ref ${args.ref}` : `selector ${args.selector}`} did not become visible in time; scroll it into view or take a new snapshot`);
      throw err;
    }
    if (args.path) {
      fs.mkdirSync(nodePath.dirname(args.path), { recursive: true });
      fs.writeFileSync(args.path, buffer);
    }
    size = imageSize(buffer, type);
    const maxPx = args.max_px || 1568;
    const longEdge = Math.max(size.width, size.height);
    if (longEdge > maxPx && args.inline_path) {
      // Downscale with CDP's clip.scale (no image library needed), the same
      // region as the file: the element's box, the document or the viewport.
      const scale = maxPx / longEdge;
      if (Date.now() >= deadlineAt) fail('browser_timeout', 'the screenshot ran out of time before its inline copy');
      const metrics = await state.session.send('Page.getLayoutMetrics');
      const vis = metrics.cssVisualViewport || metrics.visualViewport;
      const content = metrics.cssContentSize || metrics.contentSize;
      let clip;
      if (element) clip = { ...element, scale };
      else if (args.full) clip = { x: 0, y: 0, width: content.width, height: content.height, scale };
      else clip = { x: vis.pageX, y: vis.pageY, width: vis.clientWidth, height: vis.clientHeight, scale };
      // a clip: its device metrics are cleared after it (see clipReady in activity.mjs)
      const send = (method, params) => state.session.send(method, params);
      const small = laneShot(state.captures, cleanCapture(send, shotParams({ type, quality, clip, beyond: Boolean(element || args.full) })), deadlineAt);
      shots.push(small);
      let answer;
      try {
        answer = await small.wait(left());
      } catch (err) {
        if (small.pending()) await clearClipEmulation(send);
        if (/Timeout/i.test(err.message)) fail('tab_not_rendered', 'the tab could not be drawn for the inline copy; try again');
        throw err;
      }
      fs.writeFileSync(args.inline_path, Buffer.from(answer.data, 'base64'));
      inlinePath = args.inline_path;
    }
    } finally {
      for (const shot of shots) shot.abandon();
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
    // What a click or key press changes besides the URL: an in-page modal (a
    // Vue/React dialog has no CDP dialog event), the title, the focus.
    const probesChanges = kind === 'click' || kind === 'press';
    // `acted` (the after probe): the element acted on, so a focus that landed
    // on it (Chromium focuses a clicked button) is not reported as moved.
    const probe = (acted = null) => withTimeout(page.evaluate(modalsFn, acted), 1000, 'probe_timeout', 'probe').catch(() => null);
    const before = probesChanges ? await probe() : null;
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
    const force = hidden;
    const started = Date.now();
    try {
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
      // Playwright's call log quotes the submitted text (`fill("…")`,
      // `elementHandle.type("…")`): only its length reaches the answer and the logs.
      err.message = redactActText(err.message, kind === 'fill' || kind === 'type' ? args.text : null);
      if (/Timeout/i.test(err.message)) {
        // The element resolved before the action: stale only when it has
        // left the page since; otherwise the action itself did not finish
        // (covered, hidden, disabled, a modal in the way).
        const elapsed = Date.now() - started;
        const targetName = args.ref || args.selector || 'the focused element';
        const url = String(page.url()).split(/[?#]/)[0];
        // A JavaScript dialog the action opened blocks the page (and the probe).
        if (await dialogStillOpen(state)) {
          logActTimeout({ kind, target: targetName, role: info.role, name: info.name, hidden, force, timeout_ms: timeout, elapsed_ms: elapsed, deadline_ms, dialog: true, url, message: err.message });
          fail('dialog_open', `a ${state.dialog.type()} dialog opened during the ${kind} ("${state.dialog.message().slice(0, 80)}"); browser dialog accept|dismiss, or the user answers it`);
        }
        // true / false when the probe answered, null when it could not tell
        // (a busy renderer): only a clear false is stale.
        const connected = handle ? await withTimeout(handle.evaluate((el) => el.isConnected), 1000, 'probe_timeout', 'probe').catch(() => null) : true;
        logActTimeout({ kind, target: targetName, role: info.role, name: info.name, hidden, force, timeout_ms: timeout, elapsed_ms: elapsed, deadline_ms, connected, url, message: err.message });
        if (args.ref && connected === false) fail('stale_ref', `ref ${args.ref} no longer resolves (the element left the page during the ${kind}); re-run browser snapshot`);
        fail('action_timeout', actTimeoutMessage({ kind, target: targetName, role: info.role, name: info.name, timeout_ms: timeout, force, hidden, connected, message: err.message }));
      }
      throw err;
    } finally {
      page.off('framenavigated', onNav);
      state.inflight--;
    }
    // A JavaScript dialog the act opened blocks the page: the probe would only
    // wait out its timeout (dialog_open says it). The handle goes along only
    // while it still lives in the main frame's document.
    let changes = {};
    if (probesChanges && !state.dialog) {
      const acted = handle && !navigated && page.url() === urlBefore && (await handle.ownerFrame().catch(() => null)) === page.mainFrame() ? handle : null;
      changes = actChanges(before, await probe(acted));
    }
    return { result: { kind, role: info.role, name: info.name, navigated: navigated || page.url() !== urlBefore, url_before: urlBefore, dispatched, ...changes }, page: await pageInfo(state) };
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

  // Fork (companion browser): a local HTML file in a sandboxed view (the
  // companion's, in the caller's group) or a window tab; http(s) and ws(s)
  // blocked unless allowed; the file and its directory watched.
  async show({ profile: name, args, deadline_ms }) {
    const profile = needProfile(name);
    const file = String(args.file || '');
    if (!nodePath.isAbsolute(file)) fail('invalid_request', 'show needs an absolute path');
    if (!fs.existsSync(file)) fail('file_not_found', `no file ${file}`);
    const url = pathToFileURL(file).href;
    const allowNetwork = Boolean(args.allow_network);
    const deadlineAt = Date.now() + Math.min(deadline_ms, 10000);
    let viewId = null;
    let targetId;
    if (profile.kind === 'companion') {
      const answer = await profile.serial(() => profile.control.call('show', { tab_id: args.tab_id || null, opened_by: args.opened_by || undefined, file, root: args.root || nodePath.dirname(file), allow_network: allowNetwork, page_api: Boolean(args.page_api) && !allowNetwork, opener: args.page_api && !allowNetwork && args.opener ? String(args.opener) : undefined, focus: Boolean(args.focus), title: args.title || null }));
      viewId = answer && answer.view_id ? String(answer.view_id) : null;
      targetId = await viewTarget(profile, answer, (u) => u === url || (viewId && u === VIEW_MARKER + viewId), deadlineAt);
      profile.control.remember(targetId, viewId);
    } else {
      targetId = await profile.serial(() => createTarget(profile, 'about:blank', !args.focus));
    }
    const state = await waitTracked(profile, targetId, deadlineAt);
    if (!allowNetwork) await blockNetwork(state).catch((err) => log('debug', `show: network block: ${err.message}`));
    state.inflight++;
    try {
      if (profile.kind === 'companion') await state.page.waitForLoadState('domcontentloaded', { timeout: 5000 }).catch(() => {});
      else {
        const response = await state.page.goto(url, { waitUntil: 'domcontentloaded', timeout: Math.max(1000, deadline_ms - 1000) })
          .catch((err) => { const failure = new OpError('navigation_failed', err.message.split('\n')[0]); failure.target = targetId; throw failure; });
        if (response) state.lastStatus = response.status();
      }
    } finally { state.inflight--; }
    if (args.focus && profile.kind !== 'companion') await quietSelect(state);
    let watching = false;
    if (args.watch !== false) {
      try { watching = watches.arm(targetId, file, () => reloadView(name, targetId)); }
      catch (err) { log('debug', `show: watch: ${err.message}`); }
    }
    return { result: { target: targetId, view_id: viewId, watching }, page: await pageInfo(state) };
  },

  // A restored HTML view (herdr asks after the companion recreated it).
  async watch({ profile: name, target, args }) {
    needPage(needProfile(name), target);
    const file = String(args.file || '');
    if (!nodePath.isAbsolute(file)) fail('invalid_request', 'watch needs an absolute path');
    return { watching: watches.arm(target, file, () => reloadView(name, target)) };
  },
  async unwatch({ target }) {
    return { removed: watches.disarm(String(target || '')) };
  },

  async dialog({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    if (!(await dialogStillOpen(state))) fail('no_dialog', 'no dialog is open on this tab');
    const profile = state.profile;
    const dialog = state.dialog;
    state.dialog = null;
    // an explicit "" is an empty answer; no text keeps the field's value
    try { if (args.accept) await dialog.accept(typeof args.text === 'string' ? args.text : undefined); else await dialog.dismiss(); }
    catch (err) {
      const why = String(err && err.message).split('\n')[0];
      // the companion did not take the answer (a timeout, its control page gone): the dialog
      // is still the tab's, unless a push closed it meanwhile
      if (dialog.companion && err && err.code !== 'no_dialog') {
        const listed = profile.dialogs.get(state.target);
        if (!state.dialog && !state.closed && listed && String(listed.id) === dialog.id) state.dialog = dialog;
        fail(err.code || 'browser_error', `the companion did not answer the dialog (${why})`);
      }
      fail('no_dialog', `the dialog was already closed (${why})`);
    }
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
    const code = err instanceof OpError || (err && err.herdr && typeof err.code === 'string') ? err.code : (/Target closed|has been closed/i.test(err.message) ? 'tab_closed' : 'browser_error');
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
