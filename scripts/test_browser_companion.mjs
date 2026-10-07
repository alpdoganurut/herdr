// Companion driver checks that need no browser: a transient miss of the
// worker (Chrome idled it out) must not latch the companion into "missing".
// Run: node scripts/test_browser_companion.mjs   (also run by
// integration::browser_assets::tests when node is on PATH)
import { Companion, SWEEP_EXPR, LOADING_EXPR, describe, probePages } from '../src/integration/assets/browser/activity.mjs';

let fetches = 0;
const sent = [];
let sweepAnswer = { unloaded: 3, reloaded: 2, left: ['Gmail – Inbox'], left_count: 1 };
let loadingAnswers = [];
function check(cond, msg) { if (!cond) { console.error(`FAIL: ${msg}`); process.exit(1); } }
const worker = { type: 'service_worker', url: 'chrome-extension://abc/sw.js', webSocketDebuggerUrl: 'ws://fake/sw' };
// The first call's lookups (one probe plus the nudge retries) all miss; later ones find the worker.
globalThis.fetch = async () => ({ json: async () => (fetches++ < 20 ? [] : [worker]) });
class FakeSocket {
  constructor() { this.readyState = 0; setTimeout(() => { this.readyState = 1; this.onopen && this.onopen(); }, 0); }
  send(text) {
    const { id, params } = JSON.parse(text);
    sent.push(params.expression);
    const value = params.expression.startsWith('herdrPing') ? 'herdr-companion/11' : params.expression.startsWith('herdrGroup') ? { groupId: 3 } : params.expression.startsWith('herdrTabs') ? [] : params.expression === SWEEP_EXPR ? sweepAnswer : params.expression === LOADING_EXPR ? loadingAnswers.shift() || { loading: 0, loading_titles: [] } : { ok: true };
    setTimeout(() => this.onmessage && this.onmessage({ data: JSON.stringify({ id, result: { result: { value } } }) }), 0);
  }
  close() { this.readyState = 3; this.onclose && this.onclose(); }
}
globalThis.WebSocket = FakeSocket;
const profile = { port: 1, browser: { newBrowserCDPSession: async () => ({ send: async () => ({ targetId: 'nudge' }), detach: async () => {} }) } };
const companion = new Companion(profile);
companion.state = 'ready';
let failed = null;
try { await companion.call('herdrPing'); } catch (err) { failed = err; }
if (!failed) { console.error('FAIL: the first call should miss the worker'); process.exit(1); }
if (companion.state !== 'ready') { console.error(`FAIL: a transient miss latched state=${companion.state}`); process.exit(1); }
const pong = await companion.call('herdrPing');
if (pong !== 'herdr-companion/11') { console.error(`FAIL: second call did not reach the worker: ${pong}`); process.exit(1); }
if (fetches < 21) { console.error('FAIL: the second call did not look for the worker again'); process.exit(1); }
console.log('ok: a transient worker miss fails one call and the next one reconnects');

// The group's collapse timer takes the directive's window as it is; 0 is
// off (no group expanded or marked, nothing to collapse); a missing,
// non-numeric or huge window means the default / the timer's cap, not a
// collapse within the test.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const calls = (fn) => sent.filter((e) => e.startsWith(fn)).length;
companion.tabIds.set('t1', 5);
await companion.touch({ target: 't1' }, { key: 'w1:p1', title: '✻ planner', color: 'blue', collapse_ms: 0 });
await sleep(30);
if (calls('herdrGroup') !== 0 || calls('herdrCollapse') !== 0) { console.error(`FAIL: collapse_ms 0 is off: no group or collapse calls, got ${calls('herdrGroup')} / ${calls('herdrCollapse')}`); process.exit(1); }
await companion.touch({ target: 't1' }, { key: 'w1:p1', title: '✻ planner', color: 'blue', collapse_ms: 40 });
if (calls('herdrGroup') !== 1) { console.error('FAIL: a 40 ms window groups the tab'); process.exit(1); }
await sleep(15);
if (calls('herdrCollapse') !== 0) { console.error('FAIL: a 40 ms window collapsed before 40 ms'); process.exit(1); }
await sleep(60);
if (calls('herdrCollapse') !== 1) { console.error(`FAIL: a 40 ms window should have collapsed by now, got ${calls('herdrCollapse')}`); process.exit(1); }
for (const window of [undefined, 'x', -5, 1e12]) {
  await companion.touch({ target: 't1' }, { key: 'w1:p1', title: '✻ planner', color: 'blue', collapse_ms: window });
  await sleep(30);
  if (calls('herdrCollapse') !== 1) { console.error(`FAIL: collapse_ms ${window} should mean the default window or the cap, got ${calls('herdrCollapse')}`); process.exit(1); }
}
// Quiet tab selection: the worker's chrome.tabs.update({ active: true }) and
// nothing that raises the app; false (no call) without a ready companion or a
// known tab id.
sent.length = 0;
companion.tabIds.set('t1', 5);
if (!(await companion.select({ target: 't1' }))) { console.error('FAIL: a known tab is selected'); process.exit(1); }
if (sent.length !== 1 || sent[0] !== 'chrome.tabs.update(5, { active: true })') { console.error(`FAIL: the select expression: ${JSON.stringify(sent)}`); process.exit(1); }
if (sent.some((e) => /windows\.update|focused|bringToFront|activateTarget/.test(e))) { console.error('FAIL: nothing may raise the window'); process.exit(1); }
companion.state = 'missing';
sent.length = 0;
if (await companion.select({ target: 't1' }) !== false || sent.length) { console.error('FAIL: no companion, no selection, no call'); process.exit(1); }
companion.state = 'ready';
if (await companion.select({ target: 'unknown', page: { url: () => 'https://nowhere.test/', title: async () => '' } }) !== false) { console.error('FAIL: an unknown tab is not selected'); process.exit(1); }
if (sent.some((e) => e.startsWith('chrome.tabs.update'))) { console.error('FAIL: an unknown tab made no update call'); process.exit(1); }
companion.close();
console.log('ok: the group collapse honours the window as given, 0 is off, a huge window is capped; tabs are selected quietly');

// ---------------------------------------------------------------- the sweep
// Before a connect: the unloaded restored tabs are reloaded through the worker
// (one expression), the reloaded ones settle, and the result carries the counts.
companion.state = 'ready';
sent.length = 0;
loadingAnswers = [{ loading: 2, loading_titles: ['a', 'b'] }, { loading: 0, loading_titles: [] }];
const t0 = Date.now();
const swept = await companion.sweep(8000);
check(swept.state === 'ready' && swept.unloaded === 3 && swept.reloaded === 2 && swept.left_count === 1 && swept.left[0] === 'Gmail – Inbox', `the sweep result carries the counts: ${JSON.stringify(swept)}`);
check(swept.loading === 0 && sent.filter((e) => e === LOADING_EXPR).length === 2 && Date.now() - t0 >= 400, `the reloaded tabs settled (two polls): ${JSON.stringify(sent)}`);
check(sent.filter((e) => e === SWEEP_EXPR).length === 1 && /status: 'unloaded'/.test(SWEEP_EXPR) && /chrome\.tabs\.reload/.test(SWEEP_EXPR) && !/discard/.test(SWEEP_EXPR), 'one sweep expression: reload every unloaded tab, no discard');
// nothing to reload: no settle polls
sent.length = 0;
sweepAnswer = { unloaded: 0, reloaded: 0, left: [], left_count: 0 };
const idle = await companion.sweep(8000);
check(idle.state === 'ready' && idle.reloaded === 0 && !sent.some((e) => e === LOADING_EXPR), 'nothing unloaded: no settle');
// no worker listed within the budget: a result, not a throw
const realFetch = globalThis.fetch;
globalThis.fetch = async () => ({ json: async () => [] });
companion.close();
const tMissing = Date.now();
const missing = await companion.sweep(1500);
check(missing.state === 'missing' && Date.now() - tMissing < 2500, `no worker: missing within the budget: ${JSON.stringify(missing)}`);
globalThis.fetch = realFetch;
// the worker's evaluate never answers: failed, bounded
companion.close();
let mute = false;
const realSend = FakeSocket.prototype.send;
FakeSocket.prototype.send = function (text) { if (mute) return; return realSend.call(this, text); };
mute = true;
const tFail = Date.now();
const muted = await companion.sweep(1200);
check(muted.state === 'failed' && Date.now() - tFail < 4000, `a mute worker: failed, bounded: ${JSON.stringify(muted)}`);
mute = false;
FakeSocket.prototype.send = realSend;
companion.close();

// SWEEP_EXPR against a fake chrome.tabs: every unloaded tab reloaded (active or not), the refusals counted
{
  const tabsList = [
    { id: 1, status: 'unloaded', active: true, title: 'Active restored' },
    { id: 2, status: 'unloaded', active: false, title: 'Gmail – Inbox' },
    { id: 3, status: 'unloaded', active: false, url: 'https://x.test/no-title' },
    { id: 4, status: 'complete', active: false, title: 'Loaded' },
  ];
  const reloaded = [];
  const chrome = { tabs: {
    query: async (q) => tabsList.filter((t) => !q.status || t.status === q.status),
    reload: async (id) => { if (id === 3) throw new Error('No tab with id: 3.'); reloaded.push(id); },
  } };
  const r = await new Function('chrome', `return ${SWEEP_EXPR}`)(chrome);
  check(r.unloaded === 3 && r.reloaded === 2 && reloaded.join(',') === '1,2' && r.left_count === 1 && r.left[0] === 'https://x.test/no-title', `the expression reloads every unloaded tab and counts the refusals: ${JSON.stringify(r)}`);
}

// describe(): one clause by precedence, never a stop
const clause1 = describe({ state: 'ready', unloaded: 3, reloaded: 2, left: ['Gmail – Inbox'], left_count: 1, loading: 0 });
const clause2 = describe({ state: 'ready', unloaded: 2, reloaded: 2, left: [], left_count: 0, loading: 0 }, [{ title: 'Checkout', host: 'shop.test' }]);
const clause3 = describe({ state: 'ready', unloaded: 2, reloaded: 2, left: [], left_count: 0, loading: 1, loading_titles: ['Slow page'] });
const clause4 = describe({ state: 'missing', detail: 'no companion service worker on the DevTools port' });
const clause5 = describe({ state: 'ready', unloaded: 3, reloaded: 3, left: [], left_count: 0, loading: 0 });
check(/^1 restored tab\(s\) could not be loaded \("Gmail – Inbox"\); ask the user to click them once/.test(clause1), clause1);
check(/^these tabs are not responding \(a dialog may be open in one\): "Checkout — shop.test"; answer or close it/.test(clause2), clause2);
check(/^1 tab\(s\) are still loading \("Slow page"\); retry in a few seconds/.test(clause3), clause3);
check(/^the companion extension is not reachable \(missing: no companion service worker/.test(clause4) && /click the restored tabs once/.test(clause4), clause4);
check(/^3 restored tab\(s\) were loaded, so a page that is not answering is blocking — a dialog/.test(clause5), clause5);
check([clause1, clause2, clause3, clause4, clause5].every((c) => !/stop/i.test(c)), 'no clause advises a stop');

// probePages(): the page targets that do not answer Runtime.evaluate are the blockers
{
  class Hang { constructor() { setTimeout(() => this.onopen && this.onopen(), 0); } send() {} close() {} }
  const open = (url) => (url.includes('hang') ? new Hang() : new FakeSocket());
  const list = [
    { type: 'page', url: 'https://shop.test/checkout', title: 'Checkout', webSocketDebuggerUrl: 'ws://fake/hang-1' },
    { type: 'page', url: 'https://ok.test/', title: 'Fine', webSocketDebuggerUrl: 'ws://fake/page-2' },
    { type: 'service_worker', url: 'chrome-extension://abc/sw.js', webSocketDebuggerUrl: 'ws://fake/sw' },
  ];
  const tProbe = Date.now();
  const probe = await probePages(1, 300, list, open);
  check(probe.pages === 2 && probe.blocked.length === 1 && probe.blocked[0].title === 'Checkout' && probe.blocked[0].host === 'shop.test' && Date.now() - tProbe < 1500, `the hanging page is the blocker, bounded: ${JSON.stringify(probe)}`);
}
console.log('ok: the sweep reloads restored tabs and settles, the attach text names what blocks, never a stop');
