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
    const value = params.expression.startsWith('herdrPing') ? 'herdr-companion/11' : params.expression.startsWith('herdrGroup') ? { groupId: 3 } : { ok: true };
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

// The group's collapse timer takes the directive's window as it is: 0 means
// the group collapses (and loses its ● mark) right after the touch; a missing
// or non-numeric window means the default, not a collapse within the test.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const collapses = () => sent.filter((e) => e.startsWith('herdrCollapse')).length;
companion.tabIds.set('t1', 5);
await companion.touch({ target: 't1' }, { key: 'w1:p1', title: '✻ planner', color: 'blue', collapse_ms: 0 });
await sleep(30);
if (collapses() !== 1) { console.error(`FAIL: collapse_ms 0 should collapse right away, got ${collapses()} collapse call(s)`); process.exit(1); }
await companion.touch({ target: 't1' }, { key: 'w1:p1', title: '✻ planner', color: 'blue', collapse_ms: 40 });
await sleep(15);
if (collapses() !== 1) { console.error('FAIL: a 40 ms window collapsed before 40 ms'); process.exit(1); }
await sleep(60);
if (collapses() !== 2) { console.error(`FAIL: a 40 ms window should have collapsed by now, got ${collapses()}`); process.exit(1); }
for (const window of [undefined, 'x', -5]) {
  await companion.touch({ target: 't1' }, { key: 'w1:p1', title: '✻ planner', color: 'blue', collapse_ms: window });
  await sleep(30);
  if (collapses() !== 2) { console.error(`FAIL: collapse_ms ${window} should mean the default window, got ${collapses()}`); process.exit(1); }
}
companion.close();
console.log('ok: the group collapse honours the window as given, 0 included');
