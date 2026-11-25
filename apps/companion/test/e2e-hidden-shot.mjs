// A screenshot of a companion tab that is not on screen must work (fork, 2026-11-15): a
// loaded tab in a herdr tab the companion is not showing (shown there before, or never), a
// non-selected tab of the shown herdr tab, a web page. Chromium draws no frame for a hidden
// view: a CDP Page.captureScreenshot of one waits for a frame that never comes, from a few
// seconds after the view was hidden (a view that was never shown has no size at all, 0x0).
// So every case waits HIDDEN_SETTLE_MS after its tab was hidden before it is captured. The
// capture is the host's own (activity.mjs paintHold around firstShot and the scaled inline
// copy, as host.mjs screenshot takes them) on a raw CDP session, as the host's. None may need
// the quiet select (on the shown herdr tab it changes the page the user sees), and the user's
// shown page must stay on top during the capture (the stacking order) and look the same.
// A full-page shot of a never-shown tab must also leave it drawing right once shown (the
// capture-race protections, test/e2e-capture-race.mjs).
// Two overlapping shots of one tab share its paint holds (the first one's end keeps the
// second's), and a shown page with no CSS background stays opaque over a held view.
//
// Starts ONE headless Electron (at a Retina scale factor: the inline copy is taken too), quits
// it through its control page. Run: node test/e2e-hidden-shot.mjs
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createMockHerdr } from './mock-herdr.mjs';
import { reapOnExit, awaitExit } from './e2e-reap.mjs';
import * as activity from '../../../src/integration/assets/browser/activity.mjs';

const require = createRequire(import.meta.url);
const electronBin = require('electron');
const { Connection } = require('../src/herdr-api.js');
const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

// how long a tab stays hidden before it is captured (a hidden view stops drawing after ~3 s)
const HIDDEN_SETTLE_MS = 5000;

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'hc-e2e-hidden-'));
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
// the companion's control page as the host's ControlPage answers it (an {error} answer throws)
const control = (viewId) => ({
  viewOf: async () => viewId,
  call: async (fn, arg) => {
    const out = await ctl(`typeof __herdrCompanion.${fn} === 'function' ? __herdrCompanion.${fn}(${JSON.stringify(arg)}) : { error: { code: 'host_verify_failed', message: 'no ${fn}()', missing: true } }`);
    if (out && out.error) throw Object.assign(new Error(out.error.message || out.error.code), { code: out.error.code, missing: !!out.error.missing });
    return out;
  },
});

const FRONT_RGB = [51, 170, 119];
let frontRgb = FRONT_RGB; // the shown page's colour (white for a page with no CSS background)
let files = 0;
const pageFile = (title, color = '#a37') => {
  const f = path.join(tmp, `p${++files}.html`);
  fs.writeFileSync(f, `<!doctype html><title>${title}</title><style>html,body{margin:0;background:${color}}</style><body><h1>${title}</h1>${'<p>line</p>'.repeat(80)}</body>`);
  return f;
};
// 100 px bands of their own colours, a 1200 px minimum width (as e2e-capture-race.mjs)
const band = (k) => [(k * 7) % 256, 255 - ((k * 11) % 256), (k * 29) % 256];
const bandsFile = () => {
  const f = path.join(tmp, `p${++files}.html`);
  fs.writeFileSync(f, `<!doctype html><title>bands</title><style>html,body{margin:0}body{min-width:1200px}div{height:100px}</style><body>${Array.from({ length: 60 }, (_, k) => `<div style="background:rgb(${band(k).join(',')})"></div>`).join('')}</body>`);
  return f;
};

let child = null;
reapOnExit(() => [child]);
const logFile = path.join(tmp, 'companion.log');
function startCompanion() {
  const out = fs.openSync(logFile, 'a');
  child = spawn(electronBin, [appDir, '--socket', sock, '--profile-dir', profile, '--headless', '--force-device-scale-factor=2'], {
    env: { ...process.env, HERDR_COMPANION_HEADLESS: '1', HERDR_COMPANION_TEST_HOOKS: '1', HERDR_SOCKET_PATH: '' },
    stdio: ['ignore', out, out],
  });
  child.exited = new Promise((r) => child.on('exit', (code) => r(code)));
}

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

const near = (a, b) => a && b && Math.abs(a[0] - b[0]) <= 6 && Math.abs(a[1] - b[1]) <= 6 && Math.abs(a[2] - b[2]) <= 6;
// the compositor's pixel is opaque (view_pixels answers RGBA)
const opaque = (p) => p && p[3] === 255;
let frontId = null;
// what the user sees of the shown page (the compositor's pixels), at three points
async function frontLooksRight() {
  const px = await call('view_pixels', { view_id: frontId, points: [[40, 300], [600, 500], [1100, 700]] });
  return !!(px && px.pixels && px.pixels.every((p) => near(p, frontRgb) && opaque(p)));
}
// view_pixels is the front view's own surface (what is above it does not show there): the
// stacking order says whether anything visible sits above the shown page. While `viewId` is
// held for a capture it is visible and under the shown page; never anything visible above it.
async function coveredWhileHeld(viewId) {
  const stack = await call('view_stack');
  const front = stack.findIndex((x) => x.view_id === frontId);
  const held = stack.findIndex((x) => x.view_id === viewId);
  const aboveFront = stack.slice(front + 1).filter((x) => x.visible).map((x) => x.view_id);
  return front >= 0 && held >= 0 && held < front && stack[held].visible && aboveFront.length === 0 && (await frontLooksRight());
}
// After the capture: only the shown page is visible.
async function onlyFrontVisible() {
  const visible = (await call('view_stack')).filter((x) => x.visible).map((x) => x.view_id);
  return JSON.stringify(visible) === JSON.stringify([frontId]);
}

const jpegSize = (buf) => {
  for (let i = 2; i < buf.length - 9; ) {
    if (buf[i] !== 0xff) break;
    const marker = buf[i + 1];
    if (marker === 0xc0 || marker === 0xc2) return { width: buf.readUInt16BE(i + 7), height: buf.readUInt16BE(i + 5) };
    i += 2 + buf.readUInt16BE(i + 2);
  }
  return { width: 0, height: 0 };
};

// The host's screenshot op of `s` (host.mjs screenshot): the paint hold, its first capture
// (the viewport, or the whole document with `full`), then the scaled inline copy when the
// picture is larger than 1568 px, then the hold let go. `selects` counts quiet selects;
// `during` runs while the hold is on (the user's view is checked there).
// a page's capture lane and paint holds, shared by its ops (the host's page state)
const lanes = new Map();
const holdsByView = new Map();
const laneOf = (viewId) => lanes.get(viewId) || lanes.set(viewId, new activity.CaptureLane()).get(viewId);
const holdsOf = (viewId) => holdsByView.get(viewId) || holdsByView.set(viewId, new activity.PaintHolds({ control: control(viewId), target: viewId, log: (t) => console.log('     ', t) })).get(viewId);
async function hostShot(s, viewId, { full = false, selects = { n: 0 }, during = null } = {}) {
  const shots = [];
  const lane = laneOf(viewId);
  const deadlineAt = Date.now() + 30000;
  const log = () => {};
  const select = () => {
    selects.n++;
    return call('select', viewId).then((r) => !(r && r.error));
  };
  const t0 = Date.now();
  const hold = await holdsOf(viewId).acquire(deadlineAt);
  const held = hold.held;
  let duringOk = null;
  try {
    if (during) duringOk = await during(viewId);
    const answer = await activity.firstShot({ send: s.send, lane, type: 'jpeg', quality: 70, full, select, log, deadlineAt, stallMs: 5000, shots });
    const first = Date.now() - t0;
    const size = jpegSize(Buffer.from(answer.data, 'base64'));
    let inline = null;
    if (Math.max(size.width, size.height) > 1568) {
      const metrics = await s.send('Page.getLayoutMetrics');
      const vis = metrics.cssVisualViewport || metrics.visualViewport;
      const content = metrics.cssContentSize || metrics.contentSize;
      const scale = 1568 / Math.max(size.width, size.height);
      const clip = full ? { x: 0, y: 0, width: content.width, height: content.height, scale } : { x: vis.pageX, y: vis.pageY, width: vis.clientWidth, height: vis.clientHeight, scale };
      const small = activity.laneShot(lane, activity.cleanCapture(s.send, activity.shotParams({ type: 'jpeg', quality: 70, clip, beyond: full }), log), deadlineAt);
      shots.push(small);
      inline = (await small.wait(Math.max(1000, deadlineAt - Date.now()))).data.length;
    }
    return { bytes: answer.data.length, size: `${size.width}x${size.height}`, inline, held, during: duringOk, first_ms: first, ms: Date.now() - t0 };
  } catch (err) {
    return { err: err.message, code: err.code || null, held, during: duringOk, ms: Date.now() - t0 };
  } finally {
    for (const shot of shots) shot.abandon();
    await holdsOf(viewId).release(hold.token);
  }
}

async function newView(start) {
  const before = new Set((await targets()).map((t) => t.id));
  const answer = start();
  const target = await waitFor(async () => (await targets()).find((t) => !before.has(t.id) && t.type === 'page'), 5000, 2);
  if (!target) throw new Error('no new target');
  return { target, answer, s: await session(target) };
}
const loaded = (s, title) => waitFor(() => s.evaluate(`document.readyState === 'complete' && document.title === ${JSON.stringify(title)}`), 5000);
const shownViews = async () => ((await ctl('__herdrCompanion.views()')) || []).filter((x) => x.shown).map((x) => x.view_id);

// Two viewport shots in a row: drawn without the quiet select, the user's page unchanged
// during and after, the tab still hidden after.
async function twoShots(name, v, viewId) {
  for (const k of [1, 2]) {
    const selects = { n: 0 };
    const shot = await hostShot(v.s, viewId, { selects, during: coveredWhileHeld });
    const after = await shownViews();
    const front = (await frontLooksRight()) && (await onlyFrontVisible());
    check(`${name} (shot ${k}): drawn, no quiet select, held under the shown page, the user's page unchanged`, shot.bytes > 1000 && selects.n === 0 && shot.during === true && front && JSON.stringify(after) === JSON.stringify([frontId]), { shot, selects: selects.n, after, front });
    if (k === 1) await sleep(HIDDEN_SETTLE_MS);
  }
}

let other = null; // case 1's tab (hidden, in w1:t2)
try {
  startCompanion();
  check('the companion registers', await waitFor(() => mock.state.lease, 40000));
  port = Number(fs.readFileSync(path.join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]);
  // w1:t1 is the shown herdr tab, its first page selected
  const front = await newView(() => call('show', { tab_id: 'w1:t1', file: pageFile('front', `rgb(${FRONT_RGB.join(',')})`), focus: true }));
  frontId = (await front.answer).view_id;
  await loaded(front.s, 'front');
  await sleep(300);
  check('the front page is the shown one, drawn', JSON.stringify(await shownViews()) === JSON.stringify([frontId]) && (await frontLooksRight()), await shownViews());

  // 1. A loaded tab in a herdr tab the companion is not showing (w1:t2), shown there once
  //    (the user looked at it) and then left.
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t2', file: pageFile('other'), focus: false }));
    const id = (await v.answer).view_id;
    await loaded(v.s, 'other');
    await api('mock.focus_tab', { tab_id: 'w1:t2' });
    await waitFor(async () => (await shownViews()).includes(id), 3000);
    await sleep(300);
    await api('mock.focus_tab', { tab_id: 'w1:t1' });
    await waitFor(async () => (await shownViews()).includes(frontId), 3000);
    await sleep(HIDDEN_SETTLE_MS);
    await twoShots('a tab in a herdr tab not shown, shown there before', v, id);
    other = { v, id };
  }

  // 2. A loaded tab in a herdr tab never shown (an agent's tab in its own herdr tab).
  {
    const { tab_id } = await api('mock.create_tab', { label: 'never' });
    const v = await newView(() => call('show', { tab_id, file: pageFile('never'), focus: false }));
    const id = (await v.answer).view_id;
    await loaded(v.s, 'never');
    await sleep(HIDDEN_SETTLE_MS);
    await twoShots('a tab in a herdr tab never shown', v, id);
  }

  // 3. A non-selected tab of the shown herdr tab (opened in the background), loaded.
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t1', file: pageFile('behind'), focus: false }));
    const id = (await v.answer).view_id;
    await loaded(v.s, 'behind');
    await sleep(HIDDEN_SETTLE_MS);
    await twoShots('a non-selected tab of the shown herdr tab', v, id);
  }

  // 4. A web page (the shared web partition) in a herdr tab not shown.
  {
    const { tab_id } = await api('mock.create_tab', { label: 'web' });
    const v = await newView(() => call('open', { tab_id, url: 'data:text/html,<title>web</title><body style="background:%23a37">web</body>', background: true }));
    const id = (await v.answer).view_id;
    await loaded(v.s, 'web');
    await sleep(HIDDEN_SETTLE_MS);
    await twoShots('a web page in a herdr tab not shown', v, id);
  }

  // 6. Two overlapping screenshots of one hidden tab: the first one ends (and lets go of its
  //    hold) while the second has not captured yet; the second's hold keeps the tab painting.
  {
    await sleep(HIDDEN_SETTLE_MS);
    let firstDone;
    const done = new Promise((r) => { firstDone = r; });
    const selectsB = { n: 0 };
    const second = hostShot(other.v.s, other.id, {
      selects: selectsB,
      // after the first op is over, longer than a hidden view keeps drawing
      during: async (viewId) => { await done; await sleep(HIDDEN_SETTLE_MS); return coveredWhileHeld(viewId); },
    });
    const selectsA = { n: 0 };
    const first = await hostShot(other.v.s, other.id, { selects: selectsA, during: coveredWhileHeld });
    firstDone();
    const last = await second;
    check('two overlapping shots of one hidden tab: both drawn, no quiet select, the second still held after the first ends', first.bytes > 1000 && last.bytes > 1000 && selectsA.n + selectsB.n === 0 && first.during === true && last.during === true && (await onlyFrontVisible()), { first, second: last, selects: selectsA.n + selectsB.n });
  }

  // 7. The shown page has no CSS background: it is still opaque (white, alpha 255) over a
  //    view held for a capture.
  {
    const f = path.join(tmp, `p${++files}.html`);
    fs.writeFileSync(f, '<!doctype html><title>plain</title><p>plain</p>');
    const plain = await newView(() => call('show', { tab_id: 'w1:t1', file: f, focus: true }));
    frontId = (await plain.answer).view_id;
    frontRgb = [255, 255, 255];
    await loaded(plain.s, 'plain');
    await waitFor(async () => JSON.stringify(await shownViews()) === JSON.stringify([frontId]), 3000);
    await sleep(HIDDEN_SETTLE_MS);
    const selects = { n: 0 };
    const shot = await hostShot(other.v.s, other.id, { selects, during: coveredWhileHeld });
    check('a shown page with no CSS background stays opaque white over a held tab', shot.bytes > 1000 && selects.n === 0 && shot.during === true && (await onlyFrontVisible()), { shot, selects: selects.n });
  }

  // 5. A full-page shot of a never-shown tab of the shown herdr tab: drawn, and once the user
  //    selects it and scrolls, it draws its scrolled frame (no device metrics left behind).
  {
    const v = await newView(() => call('show', { tab_id: 'w1:t1', file: bandsFile(), focus: false }));
    const id = (await v.answer).view_id;
    await loaded(v.s, 'bands');
    await sleep(HIDDEN_SETTLE_MS);
    const selects = { n: 0 };
    const shot = await hostShot(v.s, id, { full: true, selects, during: coveredWhileHeld });
    await call('select', id);
    await sleep(300);
    await v.s.evaluate('window.scrollTo(0, 1000); new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))');
    await sleep(300);
    const m = await v.s.evaluate('({ scrollY, innerWidth, innerHeight })');
    const points = [[40, 5], [40, 250], [40, 555]];
    const px = await call('view_pixels', { view_id: id, points });
    const want = points.map(([, py]) => band(Math.floor((m.scrollY + py) / 100)));
    const drawn = !!(px && px.pixels && px.pixels.every((p, i) => near(p, want[i])));
    check('a full-page shot of a never-shown tab: drawn, no quiet select; shown and scrolled, it draws its scrolled frame', shot.bytes > 1000 && selects.n === 0 && shot.during === true && drawn, { shot, selects: selects.n, m, pixels: px && px.pixels, want });
  }
} catch (e) {
  check('no exception', false, String(e && e.stack));
} finally {
  try {
    await ctl('__herdrCompanion.quit()');
  } catch {}
  const code = child ? await awaitExit(child, child.exited) : 0;
  if (code === 'timeout') console.log(`companion did not exit, even on SIGTERM: pid ${child.pid}`);
  await mock.close().catch(() => {});
  console.log(failed ? `${failed} FAILED (log ${logFile})` : 'all ok');
  process.exit(failed ? 1 : 0);
}
