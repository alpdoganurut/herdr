// A clipped screenshot (full page, element) of a view that has never been shown must not
// leave that view drawing offset (fork, 2026-11-09). Chromium's clipped
// Page.captureScreenshot emulates the clip as a viewport offset and scale on the host's CDP
// session and, when the capture settles, restores the emulation only when the view had a
// size at request time (page_handler.cc ScreenshotCaptured: `if
// (original_view_size.width())`). A WebContentsView that was never shown has no size in its
// renderer (innerWidth/innerHeight 0x0, whatever its bounds), so the offset stays: once
// shown, the tab presents every frame translated by its scroll offset (a blank band grows at
// the top as the user scrolls), through reloads, until the session goes. The same happens
// when a new document commits while a clipped capture is pending (a navigation, a reload, the
// companion's about:blank marker giving way to the page right after a show): Chromium never
// answers that capture, so it never takes the emulation back. From inside the page
// and in CDP captures everything looks right; webContents.capturePage (the view_pixels test
// hook) shows what the user sees. A page with intrinsic width (min-width, fixed px, tables)
// has a non-empty document at 0x0; a flowing one fails at once ("0 width").
//
// The capture goes through the host's own first-capture code (activity.mjs firstShot) on a
// raw CDP session, as the host's. Starts ONE headless Electron, quits it through its control
// page. Run: node test/e2e-capture-race.mjs
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createMockHerdr } from './mock-herdr.mjs';
import { firstShot, CaptureLane } from '../../../src/integration/assets/browser/activity.mjs';

const require = createRequire(import.meta.url);
const electronBin = require('electron');
const { Connection } = require('../src/herdr-api.js');
const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'hc-e2e-capture-'));
const sock = path.join(tmp, 'h.sock');
const profile = path.join(tmp, 'profile');
let failed = 0;
const check = (name, ok, detail) => {
  if (!ok) failed++;
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail !== undefined ? ' — ' + JSON.stringify(detail).slice(0, 500) : ''}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, ms = 10000, step = 50) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {}
    await sleep(step);
  }
  return null;
}

const mock = createMockHerdr({ socketPath: sock, verifyCdp: true, heartbeatMs: 1000, ttlMs: 8000 });
await mock.listen();
async function api(method, params) {
  const client = new Connection(sock, { prefix: 't' });
  client.on('error', () => {});
  await client.connect();
  try {
    return await client.request(method, params);
  } finally {
    client.close();
  }
}
const ctl = async (expr) => (await api('mock.control', { expression: expr })).value;
const call = (op, arg) => ctl(`__herdrCompanion.${op}(${JSON.stringify(arg)})`);

// 100 px bands of their own colours, a 1200 px minimum width (an agent's page often has one)
const band = (k) => [(k * 7) % 256, 255 - ((k * 11) % 256), (k * 29) % 256];
const html = `<!doctype html><title>bands</title><style>html,body{margin:0}body{min-width:1200px}div{height:100px}</style><body>${Array.from({ length: 60 }, (_, k) => `<div style="background:rgb(${band(k).join(',')})"></div>`).join('')}</body>`;
let files = 0;
const pageFile = () => {
  const f = path.join(tmp, `p${++files}.html`);
  fs.writeFileSync(f, html);
  return f;
};

let child = null;
const logFile = path.join(tmp, 'companion.log');
function startCompanion() {
  const out = fs.openSync(logFile, 'a');
  child = spawn(electronBin, [appDir, '--socket', sock, '--profile-dir', profile, '--headless'], {
    env: { ...process.env, HERDR_COMPANION_HEADLESS: '1', HERDR_COMPANION_TEST_HOOKS: '1', HERDR_SOCKET_PATH: '' },
    stdio: ['ignore', out, out],
  });
  child.exited = new Promise((r) => child.on('exit', (code) => r(code)));
}

// raw CDP on one page target (the host's session is one too)
let port = 0;
const targets = async () => (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
async function session(target) {
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  let id = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    const p = m.id && pending.get(m.id);
    if (!p) return;
    pending.delete(m.id);
    if (m.error) p.reject(new Error(m.error.message));
    else p.resolve(m.result);
  };
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const i = ++id;
      pending.set(i, { resolve, reject });
      ws.send(JSON.stringify({ id: i, method, params }));
    });
  const evaluate = async (expression) => (await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })).result.value;
  return { send, evaluate, close: () => ws.close() };
}

// The host's full-page screenshot of `s` (its first capture), as the screenshot op makes it.
async function hostFullShot(s, viewId) {
  const shots = [];
  try {
    const answer = await firstShot({
      send: s.send,
      lane: new CaptureLane(),
      type: 'jpeg',
      quality: 70,
      full: true,
      select: () => (viewId ? call('select', viewId).then((r) => !(r && r.error)) : Promise.resolve(false)),
      deadlineAt: Date.now() + 15000,
      stallMs: 5000,
      shots,
    });
    return { bytes: answer.data.length };
  } catch (err) {
    return { err: err.message, code: err.code || null };
  } finally {
    for (const shot of shots) shot.abandon();
  }
}

const near = (a, b) => a && b && Math.abs(a[0] - b[0]) <= 6 && Math.abs(a[1] - b[1]) <= 6 && Math.abs(a[2] - b[2]) <= 6;
// What the user sees of a view scrolled to `y`: the compositor's pixels (capturePage) near
// the top, in the middle and near the bottom, against the bands the page has there.
async function presented(s, viewId, y) {
  await s.evaluate(`window.scrollTo(0, ${y}); new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))`);
  await sleep(300);
  const m = await s.evaluate('({ scrollY, innerWidth, innerHeight })');
  const points = [[40, 5], [40, 250], [40, 555]];
  const px = await call('view_pixels', { view_id: viewId, points });
  const want = points.map(([, py]) => band(Math.floor((m.scrollY + py) / 100)));
  const ok = !!(px && px.pixels && px.visible !== false && px.pixels.every((p, i) => near(p, want[i])));
  return { ok, m, pixels: px && px.pixels, visible: px && px.visible, want };
}

// A new view the companion is showing: its target (as soon as Chromium lists it), its session.
async function newView(start) {
  const before = new Set((await targets()).map((t) => t.id));
  const answer = start();
  const target = await waitFor(async () => (await targets()).find((t) => !before.has(t.id) && t.type === 'page'), 5000, 2);
  if (!target) throw new Error('no new target');
  return { target, answer, s: await session(target) };
}
const loaded = (s) => waitFor(() => s.evaluate("document.readyState === 'complete' && document.title === 'bands'"), 5000);

try {
  startCompanion();
  check('the companion registers', await waitFor(() => mock.state.lease, 40000));
  port = Number(fs.readFileSync(path.join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]);
  await call('open', { tab_id: 'w1:t1', url: 'data:text/html,<title>one</title>one', background: false });

  // 1. A full-page shot of a loaded, shown tab, then a scroll: drawn right.
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t1', file: pageFile(), focus: true }));
    const shown = await v.answer;
    await loaded(v.s);
    const shot = await hostFullShot(v.s, shown.view_id);
    const r = await presented(v.s, shown.view_id, 1000);
    check('a full-page shot of a shown tab, then a scroll: the tab draws its scrolled frame', shot.bytes && r.ok, { shot, r });
    v.s.close();
  }

  // 2. show(focus) in the shown group and, at once, a full-page shot of the new tab: the view
  //    is laid out and visible from its first layout (its entry is active before it is
  //    created), so there is no pre-layout size to race.
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t1', file: pageFile(), focus: true }));
    const shotting = hostFullShot(v.s, null);
    const shown = await v.answer;
    const shot = await shotting;
    await loaded(v.s);
    const r = await presented(v.s, shown.view_id, 1000);
    check('show(focus) racing a full-page shot in the shown group: the tab draws its scrolled frame', r.ok, { shot, r });
    v.s.close();
  }

  // 3. A new document while a full-page shot is pending (at show time: the companion's
  //    about:blank marker giving way to the file; here a reload). Chromium never answers the
  //    capture, so it never takes its emulation back.
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t1', file: pageFile(), focus: true }));
    const shown = await v.answer;
    await loaded(v.s);
    const shotting = hostFullShot(v.s, null);
    await waitFor(() => v.s.evaluate('1'), 1000, 1); // the capture is on its way
    v.s.send('Page.reload').catch(() => {});
    const shot = await shotting;
    await loaded(v.s);
    const r = await presented(v.s, shown.view_id, 1000);
    check('a reload during a full-page shot: the tab draws its scrolled frame', r.ok, { shot, r });
    v.s.close();
  }

  // 4. The bug: a full-page shot of a new tab that has not been shown yet (stand-in for a
  //    show(focus) the companion does not show: docked over another herdr tab, ctlFocus only
  //    marks it). Then the user shows it and scrolls.
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t1', file: pageFile(), focus: false }));
    const shown = await v.answer;
    await loaded(v.s);
    const before = await v.s.evaluate('innerWidth + "x" + innerHeight');
    const shot = await hostFullShot(v.s, shown.view_id);
    await call('select', shown.view_id);
    await sleep(300);
    const top = await presented(v.s, shown.view_id, 0);
    const r = await presented(v.s, shown.view_id, 1000);
    check('a full-page shot of a never-shown tab, then it is shown and scrolled: the tab draws its scrolled frame', top.ok && r.ok, { before, shot, top: top.ok, r });
    // a reload does not bring it back (the emulation is the session's, not the page's)
    if (!r.ok) {
      await v.s.send('Page.reload');
      await loaded(v.s);
      console.log('     after a reload:', JSON.stringify((await presented(v.s, shown.view_id, 1000)).pixels));
    }
    v.s.close();
  }

  // 5. A never-shown tab in a herdr tab the companion is not showing: the quiet select cannot
  //    show it. The full-page shot must not be taken at 0x0 (it fails as not rendered); once the
  //    user goes to that herdr tab the page draws right.
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t2', file: pageFile(), focus: false }));
    const shown = await v.answer;
    await loaded(v.s);
    const shot = await hostFullShot(v.s, shown.view_id);
    await api('mock.focus_tab', { tab_id: 'w1:t2' });
    await waitFor(async () => (await ctl('__herdrCompanion.views()')).find((x) => x.view_id === shown.view_id && x.shown), 3000);
    await sleep(300);
    const r = await presented(v.s, shown.view_id, 1000);
    check('a full-page shot of a tab in a herdr tab not shown: then shown and scrolled, it draws its scrolled frame', r.ok, { shot, r });
    v.s.close();
  }
} catch (e) {
  check('no exception', false, String(e && e.stack));
} finally {
  try {
    await ctl('__herdrCompanion.quit()');
  } catch {}
  const code = child ? await Promise.race([child.exited, sleep(30000).then(() => 'timeout')]) : 0;
  if (code === 'timeout') console.log(`companion did not exit; abandoned pid ${child.pid}`);
  await mock.close().catch(() => {});
  console.log(failed ? `${failed} FAILED (log ${logFile})` : 'all ok');
  process.exit(failed ? 1 : 0);
}
