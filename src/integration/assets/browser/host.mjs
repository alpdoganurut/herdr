// herdr browser sidecar: one Node process per herdr server, JSON lines on stdio.
// Attaches to herdr-launched Chromium over CDP with playwright-core
// (connectOverCDP, noDefaults) and runs read operations on its tabs. Holds no
// durable state: ring buffers and refs are "since attach".
//
// Request  {"id":1,"op":"read","profile":"main","target":"<targetId>","args":{...},"deadline_ms":30000}
// Reply    {"id":1,"ok":true,"result":{...},"page":{"url":"…","title":"…","dialog_open":false}}
//          {"id":1,"ok":false,"error":{"code":"…","message":"…"}}
// Events   {"event":"tab"|"dialog"|"browser"|"log", ...}
import { chromium } from 'playwright-core';
import { createRequire } from 'node:module';
import readline from 'node:readline';
import fs from 'node:fs';
import { EXTRACT_SOURCE, LINKS_SOURCE } from './extract.mjs';

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
  if (profile.pending.has(page)) return profile.pending.get(page);
  const promise = (async () => {
    const { targetId, session } = await targetIdOf(profile, page);
    const state = new PageState(profile, page, targetId);
    profile.pages.set(targetId, state);
    state.session = session;
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
      event('tab', { profile: profile.name, target: targetId, kind: 'navigated', url: frame.url(), title: '', initiator: state.inflight > 0 ? 'command' : 'other' });
    });
    page.on('load', async () => {
      const title = await page.title().catch(() => '');
      if (title) event('tab', { profile: profile.name, target: targetId, kind: 'title', title });
    });
    page.on('close', () => {
      state.closed = true;
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

async function attach(profile, port, deadlineMs) {
  if (profile.browser && profile.browser.isConnected()) {
    return tabs(profile);
  }
  profile.port = port;
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true, timeout: Math.max(1000, deadlineMs - 500) });
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
    for (const [target] of profile.pages) profile.pages.delete(target);
    event('browser', { profile: profile.name, kind: 'disconnected', detail: 'CDP connection closed' });
  });
  await Promise.all(ctx.pages().map(page => track(profile, page, null).catch((err) => log('warn', `track failed: ${err.message}`))));
  event('browser', { profile: profile.name, kind: 'attached', detail: browser.version() });
  return tabs(profile);
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
async function pageInfo(state) {
  let title = '';
  if (!state.dialog) title = await withTimeout(state.page.title(), 800, 'x', 'x').catch(() => '');
  return { url: state.page.url(), title, dialog_open: Boolean(state.dialog), status: state.lastStatus || null };
}
function waitUntilOf(wait) {
  if (wait === 'load' || wait === 'networkidle' || wait === 'commit') return wait;
  return 'domcontentloaded';
}
async function settle(state, wait) {
  if (wait === 'networkidle' || wait === 'load') return;
  // domcontentloaded + up to 1.5 s of network quiet, best effort
  await state.page.waitForLoadState('networkidle', { timeout: 1500 }).catch(() => {});
}
// Retry once when the page navigated mid-op (user or script).
async function withRetry(state, fn) {
  try {
    return { value: await fn(), navigated_during: false };
  } catch (err) {
    if (/Execution context was destroyed|navigation|detached/i.test(err.message)) {
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
function guardDialog(state) {
  if (state.dialog) fail('dialog_open', `a ${state.dialog.type()} dialog is open on this tab ("${state.dialog.message().slice(0, 80)}"); browser dialog accept|dismiss, or the user answers it`);
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
    return attach(profile, args.port, deadline_ms);
  },
  async detach({ profile: name }) {
    const profile = profiles.get(name);
    if (profile && profile.browser) await profile.browser.close().catch(() => {});
    profiles.delete(name);
    return {};
  },
  async close_browser({ profile: name }) {
    const profile = needProfile(name);
    const session = await profile.browser.newBrowserCDPSession();
    await session.send('Browser.close').catch(() => {});
    return {};
  },
  async tabs({ profile: name }) {
    return tabs(needProfile(name));
  },

  async open({ profile: name, args, deadline_ms }) {
    const profile = needProfile(name);
    const targetId = await createTarget(profile, args.url, args.background !== false);
    const deadline = Date.now() + Math.min(deadline_ms, 10000);
    let state = profile.pages.get(targetId);
    while (!state && Date.now() < deadline) { await sleep(50); state = profile.pages.get(targetId); }
    if (!state) fail('browser_start_failed', 'the new tab did not show up');
    state.inflight++;
    try {
      await state.page.waitForLoadState(waitUntilOf(args.wait), { timeout: Math.max(1000, deadline_ms - 1000) }).catch((err) => { if (!/Target closed|closed/i.test(err.message)) throw new OpError('navigation_failed', err.message.split('\n')[0]); });
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
    guardDialog(state);
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
    guardDialog(state);
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

  async focus({ profile: name, target }) {
    const state = needPage(needProfile(name), target);
    await state.page.bringToFront();
    return { result: {}, page: await pageInfo(state) };
  },

  async read({ profile: name, target, args, deadline_ms }) {
    const state = needPage(needProfile(name), target);
    guardDialog(state);
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
    guardDialog(state);
    const r = await withRetry(state, () => state.page.evaluate(linksFn, { filter: args.filter || '', max: args.max || 100 }));
    return { result: r.value, page: await pageInfo(state) };
  },

  async screenshot({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    guardDialog(state);
    const type = args.format === 'png' ? 'png' : 'jpeg';
    const options = { path: args.path, type, fullPage: Boolean(args.full), timeout: SCREENSHOT_STALL_MS };
    if (type === 'jpeg') options.quality = args.quality || 70;
    const scope = scopeLocator(state, args);
    let buffer;
    try {
      buffer = scope ? await scope.screenshot(options) : await state.page.screenshot(options);
    } catch (err) {
      if (/Timeout/i.test(err.message)) fail('tab_not_rendered', 'the tab did not paint within 5 s (background tab); rerun with --front to select it first');
      if (args.ref && /aria-ref|resolve/i.test(err.message)) fail('stale_ref', `ref ${args.ref} no longer resolves; take a new snapshot`);
      throw err;
    }
    const size = imageSize(buffer, type);
    let inlinePath = null;
    const maxPx = args.max_px || 1568;
    const longEdge = Math.max(size.width, size.height);
    if (longEdge > maxPx && args.inline_path && !scope) {
      // Downscale with CDP's clip.scale (no image library needed).
      const session = state.session;
      const scale = maxPx / longEdge;
      const metrics = await session.send('Page.getLayoutMetrics');
      const vis = metrics.cssVisualViewport || metrics.visualViewport;
      const content = metrics.cssContentSize || metrics.contentSize;
      const clip = args.full
        ? { x: 0, y: 0, width: content.width, height: content.height, scale }
        : { x: vis.pageX, y: vis.pageY, width: vis.clientWidth, height: vis.clientHeight, scale };
      const shot = await session.send('Page.captureScreenshot', { format: type, quality: type === 'jpeg' ? (args.quality || 70) : undefined, clip, captureBeyondViewport: Boolean(args.full) });
      fs.writeFileSync(args.inline_path, Buffer.from(shot.data, 'base64'));
      inlinePath = args.inline_path;
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
    guardDialog(state);
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
    guardDialog(state);
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
    guardDialog(state);
    const r = await withRetry(state, async () => {
      const value = await state.page.evaluate(args.expr);
      try { JSON.stringify(value); return value === undefined ? null : value; } catch { return String(value); }
    });
    return { result: { value: r.value, navigated_during: r.navigated_during }, page: await pageInfo(state) };
  },

  async dialog({ profile: name, target, args }) {
    const state = needPage(needProfile(name), target);
    if (!state.dialog) fail('no_dialog', 'no dialog is open on this tab');
    const dialog = state.dialog;
    state.dialog = null;
    if (args.accept) await dialog.accept(args.text || undefined); else await dialog.dismiss();
    event('dialog', { profile: name, target, state: 'closed' });
    return { result: { type: dialog.type(), message: dialog.message() }, page: await pageInfo(state) };
  },
};

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

async function handle(req) {
  const op = ops[req.op];
  if (!op) return send({ id: req.id, ok: false, error: { code: 'unknown_op', message: `unknown op ${req.op}; run herdr browser setup` } });
  const deadline = Number(req.deadline_ms || 30000);
  try {
    const out = await withTimeout(op({ profile: req.profile, target: req.target, args: req.args || {}, deadline_ms: deadline }), deadline, 'browser_timeout', `${req.op} exceeded ${deadline} ms`);
    if (out && typeof out === 'object' && 'result' in out && 'page' in out) send({ id: req.id, ok: true, result: out.result, page: out.page });
    else send({ id: req.id, ok: true, result: out || {} });
  } catch (err) {
    const code = err instanceof OpError ? err.code : (/Target closed|has been closed/i.test(err.message) ? 'tab_closed' : 'browser_error');
    send({ id: req.id, ok: false, error: { code, message: String(err && err.message || err).split('\n')[0].slice(0, 500) } });
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
