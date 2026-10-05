// Companion driver checks that need no browser: a transient miss of the
// worker (Chrome idled it out) must not latch the companion into "missing".
// Run: node scripts/test_browser_companion.mjs   (also run by
// integration::browser_assets::tests when node is on PATH)
import { Companion } from '../src/integration/assets/browser/activity.mjs';

let fetches = 0;
const sent = [];
const worker = { type: 'service_worker', url: 'chrome-extension://abc/sw.js', webSocketDebuggerUrl: 'ws://fake/sw' };
// The first call's lookups (one probe plus the nudge retries) all miss; later ones find the worker.
globalThis.fetch = async () => ({ json: async () => (fetches++ < 20 ? [] : [worker]) });
class FakeSocket {
  constructor() { this.readyState = 0; setTimeout(() => { this.readyState = 1; this.onopen && this.onopen(); }, 0); }
  send(text) {
    const { id, params } = JSON.parse(text);
    sent.push(params.expression);
    const value = params.expression.startsWith('herdrPing') ? 'herdr-companion/11' : params.expression.startsWith('herdrGroup') ? { groupId: 3 } : params.expression.startsWith('herdrTabs') ? [] : { ok: true };
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
