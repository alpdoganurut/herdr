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
import { Overlay, Companion, OVERLAY_JS, dismissPanes, FRAME_LINGER_MS, CURSOR_LINGER_MS, LINGER_MS } from '../src/integration/assets/browser/activity.mjs';

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
