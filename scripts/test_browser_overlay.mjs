// Activity overlay checks that need no browser: the frame stays FRAME_LINGER_MS
// after the last act (pulsing during an op, calmer after) and the cursor
// CURSOR_LINGER_MS after the last move, each capped by the directive's window;
// a navigation mid-window puts the frame back in the same state without
// stretching the window, a gone pane dismisses it; the companion marks an
// active group's title and unmarks it when the group collapses, and the
// worker's group code keeps its ownership rules under the mark.
// Run: node scripts/test_browser_overlay.mjs   (also run by
// integration::browser_assets::tests when node is on PATH)
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { Overlay, Companion, OVERLAY_JS, dismissPanes, captureWithQuietRetry, CaptureLane, laneShot, captureTimeout, shotParams, clipReady, cleanCapture, firstShot, FRAME_LINGER_MS, CURSOR_LINGER_MS, LINGER_MS } from '../src/integration/assets/browser/activity.mjs';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function check(cond, msg) { if (!cond) { console.error(`FAIL: ${msg}`); process.exit(1); } }

// --------------------------------------------------------------- the overlay
const evals = [];
const session = {
  async send(method, params) {
    if (method === 'Page.getFrameTree') return { frameTree: { frame: { id: 'f1' } } };
    if (method === 'Page.createIsolatedWorld') return { executionContextId: 7 };
    if (method === 'Runtime.evaluate') { evals.push(params.expression); return { result: { value: 'installed' } }; }
    throw new Error(`unexpected ${method}`);
  },
};
const overlay = new Overlay({ session });
await overlay.show('#00c8ff');
check(evals.length === 1 && evals[0].startsWith(`(${OVERLAY_JS})(`) && evals[0].endsWith(', "#00c8ff", true)'), 'show installs the frame in the busy look');
check(overlay.active && overlay.until === Infinity, 'an op in flight has no end');
await overlay.moveTo(10, 20, false);
check(overlay.cursor && overlay.cursor.x === 10 && overlay.cursor.y === 20 && overlay.cursorUntil === Infinity, 'the cursor is remembered and due until the op ends');
overlay.linger(120);
check(!overlay.active && overlay.until > Date.now() + 60 && overlay.until < Date.now() + 200, 'a short window wins over the frame linger');
check(overlay.cursorUntil > Date.now() + 60 && overlay.cursorUntil <= overlay.until, 'and over the cursor linger');
await sleep(10);
check(evals.some((e) => e.includes('busy(false)')), 'the frame settles to the calmer look when the op is done');
// a navigation inside the window: the frame comes back idle, the cursor where it was, the window untouched
const until = overlay.until;
evals.length = 0;
overlay.navigated();
await overlay.reapply();
check(evals.length === 1 && evals[0].includes('{"x":10,"y":20}') && evals[0].endsWith(', "#00c8ff", false)'), `reapply re-installs idle with the cursor: ${evals[0] && evals[0].slice(-60)}`);
check(overlay.until === until && !overlay.active, 'reapply does not stretch the window or restart the pulse');
await overlay.suspend(true);
check(evals.some((e) => e.includes('suspend(true)')), 'a screenshot hides the frame inside the window');
await sleep(170);
check(overlay.until === 0 && overlay.hideTimer === null && evals.some((e) => e.includes('hide(false)')), 'the frame fades at the end of the window');
check(overlay.cursorUntil === 0 && overlay.cursorTimer === null && evals.some((e) => e.includes('hideCursor()')), 'and the cursor with it');
evals.length = 0;
await overlay.suspend(true);
await overlay.reapply();
check(evals.length === 0, 'nothing to suspend or re-apply after the window');
// the next op pulses again; a gone pane ends it at once
await overlay.show();
check(overlay.active && evals[evals.length - 1].endsWith(', "#00c8ff", true)'), 'a new op re-shows the busy look in the kept colour');
overlay.linger(5000);
await overlay.dismiss();
check(overlay.until === 0 && overlay.hideTimer === null && !overlay.active && evals.some((e) => e.includes('hide(true)')), 'dismiss hides now and drops the timer');
evals.length = 0;
await overlay.dismiss();
check(evals.length === 0, 'a second dismiss has nothing to do');
check(FRAME_LINGER_MS === 15000 && CURSOR_LINGER_MS === 5000 && LINGER_MS === 120000, 'the approved values');
{
  // the usual window (120 s): the frame fades 15 s after the act, the cursor 5 s after the move that showed it
  const o = new Overlay({ session });
  await o.show();
  await o.moveTo(1, 2, false);
  const t = Date.now();
  o.linger(120000);
  check(o.until >= t + FRAME_LINGER_MS - 5 && o.until <= Date.now() + FRAME_LINGER_MS, `the frame goes 15 s after the act: ${o.until - t}`);
  check(o.cursorUntil >= t + CURSOR_LINGER_MS - 5 && o.cursorUntil <= Date.now() + CURSOR_LINGER_MS, `the cursor 5 s after the move: ${o.cursorUntil - t}`);
  // a later act that does not move the cursor re-shows the frame and restarts its timer, the cursor's stays
  await sleep(20);
  const cursorDue = o.cursorUntil;
  await o.show();
  o.linger(120000);
  check(o.until > t + FRAME_LINGER_MS + 10, 'every act restarts the frame timer');
  check(o.cursorUntil === cursorDue, 'an act without a move leaves the cursor timer as it was');
  // a move restarts the cursor timer
  await o.show();
  await sleep(20);
  await o.moveTo(3, 4, false);
  o.linger(120000);
  check(o.cursorUntil > cursorDue + 10, 'a move restarts the cursor timer');
  o.dispose();
}
{
  // the cursor hides on its own; the frame stays (short lingers for the check)
  const o = new Overlay({ session });
  o.frameLingerMs = 80;
  o.cursorLingerMs = 25;
  await o.show();
  await o.moveTo(5, 6, false);
  evals.length = 0;
  o.linger(120000);
  await sleep(50);
  check(evals.some((e) => e.includes('hideCursor()')) && !evals.some((e) => e.includes('hide(')), 'the cursor is hidden first, the frame is still up');
  check(o.cursorUntil === 0 && o.until > Date.now(), 'the cursor is done, the frame window still open');
  // a navigation now brings the frame back without the cursor
  evals.length = 0;
  o.navigated();
  await o.reapply();
  check(evals.length === 1 && evals[0].includes('(null, '), `the frame comes back, the cursor does not: ${evals[0] && evals[0].slice(-60)}`);
  await sleep(60);
  check(o.until === 0 && evals.some((e) => e.includes('hide(false)')), 'then the frame fades');
  // a colour change re-installs the frame in place: still without the cursor (the page keeps nothing of it)
  evals.length = 0;
  await o.show('#123456');
  check(evals.length === 1 && evals[0].includes('(null, "#123456"'), `a re-install on a colour change keeps the cursor hidden: ${evals[0] && evals[0].slice(-60)}`);
  check(!OVERLAY_JS.includes('live.cursor || cursor') && /hideCursor\(\) \{ api\.cursor = null;/.test(OVERLAY_JS), 'the page never resurrects a hidden cursor');
  o.dispose();
}
check(overlay.linger.length === 1 && (() => { const o = new Overlay({ session }); o.linger(undefined); const ok = o.until > Date.now() + FRAME_LINGER_MS - 100 && o.until <= Date.now() + FRAME_LINGER_MS; o.dispose(); return ok; })(), 'no window in the directive: the default window, so the frame linger');
check((() => { const o = new Overlay({ session }); o.linger('x'); const ok = o.until > Date.now() + FRAME_LINGER_MS - 100; o.dispose(); return ok; })(), 'a window that is not a number: the default one');
{
  // a huge window is capped at the timer's limit and does not hide early (an overflowing setTimeout fires at once)
  const huge = new Overlay({ session });
  await huge.show();
  evals.length = 0;
  huge.linger(1e12);
  check(huge.until <= Date.now() + FRAME_LINGER_MS && huge.until > Date.now() + FRAME_LINGER_MS - 100, `a huge window still means the frame linger: ${huge.until - Date.now()}`);
  check(Overlay.window(1e12) === 2 ** 31 - 1 && Overlay.window(0) === 0 && Overlay.window('x') === LINGER_MS, 'the window itself is capped at the timer limit');
  await sleep(20);
  check(!evals.some((e) => e.includes('hide(')), 'and the frame did not hide early');
  huge.dispose();
}
{
  // an explicit 0 is no linger: the frame goes right after the op
  const zero = new Overlay({ session });
  await zero.show();
  evals.length = 0;
  zero.linger(0);
  check(zero.until <= Date.now(), 'linger(0) ends the window now');
  await sleep(20);
  check(evals.some((e) => e.includes('hide(false)')), 'and the frame is hidden at once');
  zero.dispose();
}
{
  // a pane is gone while the companion is not ready and its tab was never grouped: the overlay still goes
  const gone = [];
  const page = (key, closed = false) => ({ paneKey: key, closed, overlay: { dismiss() { gone.push(key); return Promise.resolve(); } } });
  const profiles = [
    { companion: { state: 'missing' }, pages: new Map([['t1', page('w1:p1')], ['t2', page('w1:p2')], ['t3', page('w1:p1', true)], ['t4', page(null)]]) },
    { companion: { state: 'ready' }, pages: new Map([['t5', page('w1:p1')]]) },
    { companion: { state: 'ready' } },
  ];
  const n = dismissPanes(profiles, ['w1:p1']);
  check(n === 2 && gone.length === 2 && gone.every((k) => k === 'w1:p1'), `release dismisses the pane's pages in every profile, grouped or not, companion or not: ${n} ${JSON.stringify(gone)}`);
}

// ------------------------------------------------ the screenshot retry
{
  const timeout = () => Promise.reject(new Error('page.screenshot: Timeout 5000ms exceeded.'));
  const calls = [];
  // a stalled tab: one timeout, a quiet select, the second capture succeeds
  let n = 0;
  const buffer = await captureWithQuietRetry({ capture: () => { calls.push('capture'); return ++n === 1 ? timeout() : Promise.resolve('png'); }, select: async () => { calls.push('select'); return true; } });
  check(buffer === 'png' && calls.join(',') === 'capture,select,capture', `timeout → quiet select → one retry: ${calls}`);
  // no companion: no retry, tab_not_rendered without any advice about the window
  calls.length = 0;
  let failed = null;
  try { await captureWithQuietRetry({ capture: () => { calls.push('capture'); return timeout(); }, select: async () => false }); } catch (err) { failed = err; }
  check(failed && failed.code === 'tab_not_rendered' && failed.message === 'the tab could not be drawn; try again' && calls.length === 1, `not selected: ${failed && failed.message} ${calls}`);
  // still stalled after the select: the same error, exactly one retry
  calls.length = 0; failed = null;
  try { await captureWithQuietRetry({ capture: () => { calls.push('capture'); return timeout(); }, select: async () => true }); } catch (err) { failed = err; }
  check(failed && failed.code === 'tab_not_rendered' && !/front|focus|window/.test(failed.message) && calls.length === 2, `two timeouts: ${failed && failed.message} ${calls}`);
  // a select that throws counts as not selected
  failed = null;
  try { await captureWithQuietRetry({ capture: timeout, select: async () => { throw new Error('companion gone'); } }); } catch (err) { failed = err; }
  check(failed && failed.code === 'tab_not_rendered', 'a failing select: not rendered');
  // other errors pass through untouched, no select
  calls.length = 0; failed = null;
  try { await captureWithQuietRetry({ capture: () => Promise.reject(new Error('aria-ref e9 could not resolve')), select: async () => { calls.push('select'); return true; } }); } catch (err) { failed = err; }
  check(failed && !failed.code && /aria-ref/.test(failed.message) && calls.length === 0, 'a non-timeout error is not retried');
  // the op's deadline: each capture gets the stall or what is left; a short deadline means no select and no second capture
  const budgets = [];
  const grab = (ms) => { budgets.push(ms); return timeout(); };
  calls.length = 0; failed = null;
  try { await captureWithQuietRetry({ capture: (ms) => { calls.push('capture'); return grab(ms); }, select: async () => { calls.push('select'); return true; }, deadlineAt: Date.now() + 2000, stallMs: 5000 }); } catch (err) { failed = err; }
  check(failed && failed.code === 'tab_not_rendered' && calls.join(',') === 'capture' && budgets[0] <= 2000 && budgets[0] >= 1500, `a 2 s deadline: one capture within it, no select: ${calls} ${budgets}`);
  budgets.length = 0; calls.length = 0;
  const ok = await captureWithQuietRetry({ capture: (ms) => { calls.push('capture'); budgets.push(ms); return calls.length === 1 ? timeout() : Promise.resolve('png'); }, select: async () => { calls.push('select'); return true; }, deadlineAt: Date.now() + 60000, stallMs: 5000 });
  check(ok === 'png' && calls.join(',') === 'capture,select,capture' && budgets.every((b) => b === 5000), `a long deadline: the stall per capture, select, retry: ${budgets}`);
  // an element capture (ref / selector) is never retried: its timeout is the element's
  calls.length = 0; failed = null;
  try { await captureWithQuietRetry({ capture: () => { calls.push('capture'); return timeout(); }, select: async () => { calls.push('select'); return true; }, scoped: true }); } catch (err) { failed = err; }
  check(failed && !failed.code && /Timeout/.test(failed.message) && calls.join(',') === 'capture', `scoped: the original timeout, no select: ${calls}`);
}

// ------------------------------------------------ the capture lane (a stretched tab)
// Chromium puts back the view size it saw when a clipped capture arrived; one
// sent while another is pending reads the stretched size. The lane never
// sends a page's capture beside an unsettled one, and the quiet retry waits on
// the same request instead of sending a second.
{
  const lane = new CaptureLane();
  const log = [];
  let release;
  const first = lane.enqueue(() => { log.push('send 1'); return new Promise((r) => { release = r; }); });
  const second = lane.enqueue(() => { log.push('send 2'); return Promise.resolve('two'); });
  await sleep(20);
  check(log.join(',') === 'send 1', `the second capture waits for the first to settle: ${log}`);
  release('one');
  check(await first === 'one' && await second === 'two' && log.join(',') === 'send 1,send 2', `then runs: ${log}`);
  // a failed capture frees the lane too
  log.length = 0;
  const bad = lane.enqueue(() => { log.push('bad'); return Promise.reject(new Error('Unable to capture screenshot')); });
  const after = lane.enqueue(() => { log.push('after'); return Promise.resolve('ok'); });
  let failed = null;
  try { await bad; } catch (err) { failed = err; }
  check(failed && await after === 'ok' && log.join(',') === 'bad,after', `a failure frees the lane: ${log}`);

  // the stalled tab: the first wait times out, the quiet select, the retry waits on the same request
  const sends = [];
  let paint;
  const shot = laneShot(lane, () => { sends.push('capture'); return new Promise((r) => { paint = r; }); });
  const calls = [];
  const result = captureWithQuietRetry({
    capture: (ms) => { calls.push('capture'); return shot.wait(ms); },
    select: async () => { calls.push('select'); setTimeout(() => paint({ data: 'png' }), 30); return true; },
    stallMs: 60,
  });
  check((await result).data === 'png' && calls.join(',') === 'capture,select,capture' && sends.length === 1, `one request across the retry: ${calls} / ${sends}`);
  check(/Timeout/.test((await captureTimeout(new Promise(() => {}), 5).catch((e) => e)).message), 'a wait that runs out is a Timeout (the retry and element_not_visible key on it)');

  // the next screenshot of a tab whose earlier capture is still pending waits for it
  sends.length = 0;
  let late;
  const stuck = laneShot(lane, () => { sends.push('stalled'); return new Promise((r) => { late = r; }); });
  failed = null;
  try { await captureWithQuietRetry({ capture: (ms) => stuck.wait(ms), select: async () => true, stallMs: 30 }); } catch (err) { failed = err; }
  check(failed && failed.code === 'tab_not_rendered', 'still stalled after the retry: tab_not_rendered');
  stuck.abandon();
  const next = laneShot(lane, () => { sends.push('next'); return Promise.resolve({ data: 'jpeg' }); });
  const waiting = next.wait(1000);
  await sleep(20);
  check(sends.join(',') === 'stalled', `a new shot is not sent beside the pending one: ${sends}`);
  late({ data: 'late' });
  check((await waiting).data === 'jpeg' && sends.join(',') === 'stalled,next', `it goes once that one settles: ${sends}`);

  // an op that gave up before its request started: never sent
  sends.length = 0;
  let hold;
  const blocker = lane.enqueue(() => new Promise((r) => { hold = r; }));
  const gaveUp = laneShot(lane, () => { sends.push('late'); return Promise.resolve({}); });
  const gone = gaveUp.wait(10).catch((e) => e);
  check(/Timeout/.test((await gone).message), 'the wait times out behind the blocker');
  gaveUp.abandon();
  hold('done'); await blocker; await sleep(10);
  check(sends.length === 0, 'an abandoned shot never reaches Chromium');

  // past the op's deadline (the outer timeout answered already): never sent,
  // neither a shot asked for afterwards nor one queued before it
  sends.length = 0;
  const expired = await laneShot(lane, () => { sends.push('expired'); return Promise.resolve({}); }, Date.now() - 1).wait(1000).catch((e) => e);
  check(/abandoned/.test(expired.message) && sends.length === 0, `a shot asked for after its deadline is not sent: ${expired.message} / ${sends}`);
  let hold2;
  const blocker2 = lane.enqueue(() => new Promise((r) => { hold2 = r; }));
  const queued = laneShot(lane, () => { sends.push('queued'); return Promise.resolve({}); }, Date.now() + 20);
  const queuedWait = queued.wait(1000).catch((e) => e);
  await sleep(50);
  hold2('done'); await blocker2;
  check(/abandoned/.test((await queuedWait).message) && sends.length === 0, `a shot whose deadline passed while it waited in the lane is not sent: ${sends}`);
  const fresh = laneShot(lane, () => { sends.push('fresh'); return Promise.resolve({ data: 'ok' }); }, Date.now() + 5000);
  check((await fresh.wait(1000)).data === 'ok' && sends.join(',') === 'fresh', `the lane stays free for the next shot: ${sends}`);

  // one Chromium never answers does not wedge the page for good
  const short = new CaptureLane({ staleMs: 30 });
  short.enqueue(() => new Promise(() => {}));
  check(await short.enqueue(() => Promise.resolve('through')) === 'through', 'a capture starts after the stale cap');

  // the parameters: a viewport shot has no clip (Chromium leaves the view alone)
  const viewport = shotParams({ type: 'jpeg', quality: 70 });
  check(!viewport.clip && !viewport.captureBeyondViewport && viewport.quality === 70, `a viewport shot: ${JSON.stringify(viewport)}`);
  const full = shotParams({ type: 'png', quality: 70, clip: { x: 0, y: 0, width: 600, height: 9000 }, beyond: true });
  check(full.clip.scale === 1 && full.captureBeyondViewport === true && full.quality === undefined, `a full page: ${JSON.stringify(full)}`);
  console.log('ok: a page\'s captures run one at a time, the quiet retry waits on the same request, an abandoned one is never sent');
}

// ------------------------------------------- clipped captures (fork, 2026-11-09)
// A clipped capture waits for a tab with a size and its document loaded, and
// the device metrics it sets are always taken back (clipReady, cleanCapture
// in activity.mjs; live: apps/companion/test/e2e-capture-race.mjs).
{
  // a fake session: Runtime.evaluate answers the probes in turn (the last one repeats)
  const fake = (probes, extra = {}) => {
    const sent = [];
    let i = 0;
    const send = async (method, params) => {
      sent.push(method);
      if (method === 'Runtime.evaluate') {
        const p = probes[Math.min(i++, probes.length - 1)];
        if (p instanceof Error) throw p;
        return { result: { value: p } };
      }
      if (extra[method]) return extra[method](params);
      return {};
    };
    return { send, sent };
  };
  const sized = { width: 800, height: 600, loaded: true };
  let f = fake([{ width: 0, height: 0, loaded: true }, { width: 0, height: 0, loaded: true }, sized]);
  check((await clipReady({ send: f.send, pollMs: 5 })).width === 800, 'a tab that gets its size is ready');
  f = fake([{ width: 800, height: 600, loaded: false }, sized]);
  check((await clipReady({ send: f.send, pollMs: 5 })).loaded, 'a document still loading (or the companion marker) is waited for');

  // never a size: one quiet select halfway, then tab_not_rendered and no capture
  f = fake([{ width: 0, height: 0, loaded: true }]);
  const selects = [];
  const t0 = Date.now();
  let err = null;
  try { await clipReady({ send: f.send, pollMs: 5, waitMs: 200, select: () => { selects.push(Date.now() - t0); return true; } }); } catch (e) { err = e; }
  check(err && err.code === 'tab_not_rendered' && /not been shown/.test(err.message), `a tab that never gets a size is tab_not_rendered: ${err && err.message}`);
  check(selects.length === 1 && selects[0] >= 100, `selected quietly once, halfway: ${selects}`);
  // the same through firstShot: the full-page capture is never sent
  f = fake([{ width: 0, height: 0, loaded: true }]);
  err = null;
  try {
    await firstShot({ send: f.send, lane: new CaptureLane(), type: 'png', full: true, select: () => false, deadlineAt: Date.now() + 400, stallMs: 100, shots: [] });
  } catch (e) { err = e; }
  check(err && err.code === 'tab_not_rendered' && !f.sent.includes('Page.captureScreenshot'), `no capture of a tab without a size: ${f.sent}`);

  // a still loading page goes ahead at the end; a renderer that does not answer the probe too
  f = fake([{ width: 800, height: 600, loaded: false }]);
  check((await clipReady({ send: f.send, pollMs: 5, waitMs: 60 })).loaded === false, 'a page that never finishes loading goes ahead');
  f = fake([new Error('busy')]);
  check((await clipReady({ send: f.send, pollMs: 5, waitMs: 60 })) === null, 'a probe that fails does not block the capture');

  // every capture takes its device metrics back, answered or failed
  f = fake([sized], { 'Page.captureScreenshot': () => ({ data: 'ok' }) });
  check((await cleanCapture(f.send, {})()).data === 'ok' && f.sent.join() === 'Page.captureScreenshot,Emulation.clearDeviceMetricsOverride', `cleared after a capture: ${f.sent}`);
  f = fake([sized], { 'Page.captureScreenshot': () => { throw new Error('no'); } });
  err = null;
  try { await cleanCapture(f.send, {})(); } catch (e) { err = e; }
  check(err && f.sent.join() === 'Page.captureScreenshot,Emulation.clearDeviceMetricsOverride', `cleared after a failed capture: ${f.sent}`);

  // a capture Chromium never answers (a new document overtook it): pending, and
  // firstShot clears the metrics when it gives up on it
  f = fake([sized], { 'Page.getLayoutMetrics': () => ({ cssContentSize: { width: 800, height: 5000 } }), 'Page.captureScreenshot': () => new Promise(() => {}) });
  const shots = [];
  err = null;
  try {
    await firstShot({ send: f.send, lane: new CaptureLane(), type: 'png', full: true, select: () => false, deadlineAt: Date.now() + 2000, stallMs: 50, shots });
  } catch (e) { err = e; }
  check(err && err.code === 'tab_not_rendered' && shots[0].pending(), `a lost capture: ${err && err.message}`);
  check(f.sent.slice(-2).join() === 'Page.captureScreenshot,Emulation.clearDeviceMetricsOverride', `its metrics are cleared when the op gives up: ${f.sent}`);
  const done = laneShot(new CaptureLane(), async () => 'x');
  check(!done.pending(), 'a shot not sent is not pending');
  await done.wait(100);
  check(!done.pending(), 'an answered shot is not pending');
  console.log('ok: a clipped capture waits for a sized, loaded tab and always takes its device metrics back');
}

// ------------------------------------------------------------- the companion
const calls = [];
const worker = { type: 'service_worker', url: 'chrome-extension://abc/sw.js', webSocketDebuggerUrl: 'ws://fake/sw' };
globalThis.fetch = async () => ({ json: async () => [worker] });
class FakeSocket {
  constructor() { this.readyState = 0; setTimeout(() => { this.readyState = 1; this.onopen && this.onopen(); }, 0); }
  send(text) {
    const { id, params } = JSON.parse(text);
    const m = params.expression.match(/^(\w+)\((.*)\)$/s);
    const arg = JSON.parse(m[2]);
    calls.push({ fn: m[1], arg });
    const value = m[1] === 'herdrPing' ? 'herdr-companion/11' : m[1] === 'herdrGroup' ? { groupId: 3 } : { ok: true };
    setTimeout(() => this.onmessage && this.onmessage({ data: JSON.stringify({ id, result: { result: { value } } }) }), 0);
  }
  close() { this.readyState = 3; this.onclose && this.onclose(); }
}
globalThis.WebSocket = FakeSocket;
const dismissed = [];
const pages = new Map([
  ['t1', { overlay: { dismiss() { dismissed.push('t1'); return Promise.resolve(); } } }],
  ['t2', { overlay: { dismiss() { dismissed.push('t2'); return Promise.resolve(); } } }],
]);
const companion = new Companion({ port: 1, browser: null, pages });
companion.state = 'ready';
companion.tabIds.set('t1', 5);
companion.tabIds.set('t2', 6);
const group = { key: 'w1:p1', title: '✻ planner', color: 'blue', collapse_ms: 1000 };
await companion.touch({ target: 't1' }, group);
const grouped = calls.find((c) => c.fn === 'herdrGroup');
check(grouped && grouped.arg.active === true && grouped.arg.title === '✻ planner' && grouped.arg.tabId === 5, 'a touch marks the pane\'s group active, the title plain on the wire');
await sleep(1100);
const collapse = calls.find((c) => c.fn === 'herdrCollapse');
check(collapse && collapse.arg.key === 'w1:p1' && collapse.arg.title === '✻ planner', 'the collapse at the window end carries the plain title so the mark can go');
// an older worker still running (stale) gets the bare key it understands
companion.stale = true;
calls.length = 0;
await companion.touch({ target: 't1' }, group);
await sleep(1100); // (the collapse timer's floor is 1 s)
const bare = calls.find((c) => c.fn === 'herdrCollapse');
check(bare && bare.arg === 'w1:p1', `a stale worker's collapse takes the bare key: ${JSON.stringify(bare)}`);
companion.stale = false;
// the pane is gone: the overlays on its tabs go before the group dissolves; another pane's tab is left alone
await companion.touch({ target: 't1' }, group);
await companion.release(['w1:p1']);
check(dismissed.length === 1 && dismissed[0] === 't1', `release dismisses the overlay of the pane's tabs only: ${JSON.stringify(dismissed)}`);
check(calls.some((c) => c.fn === 'herdrRelease' && c.arg === 'w1:p1'), 'the worker dissolves the group');
check(!companion.groups.has('w1:p1'), 'the group is forgotten');

// ------------------------------------------------ the worker's group code
// companion.js run against a fake chrome: the marked title, the restore under
// either title, the mark removed at the collapse, the ownership rule intact.
const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, '..', 'src', 'integration', 'assets', 'browser', 'companion', 'companion.js'), 'utf8');
const tabs = new Map([[5, { id: 5, windowId: 1, groupId: -1, url: 'https://a.test/', title: 'A' }]]);
const groups = new Map();
const updates = [];
let nextGroup = 10;
const storage = { session: {}, local: {} };
const area = (name) => ({
  async get(key) { const keys = Array.isArray(key) ? key : [key]; const out = {}; for (const k of keys) if (k in storage[name]) out[k] = storage[name][k]; return out; },
  async set(obj) { Object.assign(storage[name], obj); },
});
const listener = { addListener() {} };
const chrome = {
  tabs: {
    onCreated: listener, onUpdated: listener, onRemoved: listener, onActivated: listener,
    async get(id) { const t = tabs.get(id); if (!t) throw new Error(`No tab with id: ${id}.`); return t; },
    async query(q) { return [...tabs.values()].filter((t) => q.groupId == null || t.groupId === q.groupId); },
    async group({ tabIds, groupId }) { const g = groupId == null ? nextGroup++ : groupId; if (groupId == null) groups.set(g, { id: g, windowId: 1, title: '', color: 'grey', collapsed: false }); for (const id of tabIds) tabs.get(id).groupId = g; return g; },
    async ungroup(ids) { for (const id of ids) tabs.get(id).groupId = -1; },
  },
  tabGroups: {
    onUpdated: listener,
    async get(id) { const g = groups.get(id); if (!g) throw new Error(`No group with id: ${id}.`); return g; },
    async query(q) { return [...groups.values()].filter((g) => (q.title == null || g.title === q.title) && (q.windowId == null || g.windowId === q.windowId)); },
    async update(id, props) { updates.push({ id, ...props }); Object.assign(groups.get(id), props); return groups.get(id); },
  },
  alarms: { onAlarm: listener, create() {} },
  storage: { session: area('session'), local: area('local') },
  runtime: { getURL: (p) => `chrome-extension://abc/${p}` },
  windows: { onCreated: listener, onRemoved: listener, async getAll() { return []; } },
};
const self = {};
new Function('chrome', 'self', 'globalThis', source)(chrome, self, self);
check(self.herdrPing() === 'herdr-companion/11', `the worker version moved with the code: ${self.herdrPing()}`);
let reply = await self.herdrGroup({ key: 'w1:p1', tabId: 5, title: '✻ planner', color: 'blue', wasGrouped: false, active: true });
check(reply.groupId === 10, `a new group for the pane: ${JSON.stringify(reply)}`);
check(groups.get(10).title === '● ✻ planner' && groups.get(10).collapsed === false, `the active title carries the mark: ${groups.get(10).title}`);
check(storage.local.ownedTitles && storage.local.ownedTitles['✻ planner'] && !storage.local.ownedTitles['● ✻ planner'], 'ownership goes by the plain title');
await self.herdrCollapse({ key: 'w1:p1', title: '✻ planner' });
check(groups.get(10).title === '✻ planner' && groups.get(10).collapsed === true, `the collapse removes the mark: ${groups.get(10).title}`);
reply = await self.herdrGroup({ key: 'w1:p1', tabId: 5, title: '✻ planner', color: 'blue', wasGrouped: true, active: false });
check(reply.groupId === 10 && groups.get(10).title === '✻ planner', 'a touch without the mark keeps the plain title');
await self.herdrCollapse('w1:p1');
check(groups.get(10).collapsed === true && groups.get(10).title === '✻ planner', 'an older sidecar\'s bare key still collapses');
// the browser came back mid-window: the restored group carries the marked title and a new id; the pane adopts it
storage.session = {};
groups.clear();
groups.set(42, { id: 42, windowId: 1, title: '● ✻ planner', color: 'blue', collapsed: false });
tabs.get(5).groupId = 42;
reply = await self.herdrGroup({ key: 'w1:p1', tabId: 5, title: '✻ planner', color: 'blue', wasGrouped: false, active: true });
check(reply.groupId === 42, `the pane's restored group is adopted under the marked title: ${JSON.stringify(reply)}`);
// a group of the user's with a different title is never touched
groups.set(43, { id: 43, windowId: 1, title: 'mine', color: 'red', collapsed: false });
tabs.set(6, { id: 6, windowId: 1, groupId: 43, url: 'https://b.test/', title: 'B' });
reply = await self.herdrGroup({ key: 'w1:p2', tabId: 6, title: '◇ review', color: 'cyan', wasGrouped: false, active: true });
check(reply.skipped === 'other_group' && groups.get(43).title === 'mine', `another group is left alone: ${JSON.stringify(reply)}`);
console.log('ok: the frame lives for the activity window, the cursor stays, a gone pane dismisses it, the group title carries the mark while active');
