// Companion driver checks that need no browser: a transient miss of the
// worker (Chrome idled it out) must not latch the companion into "missing".
// Run: node scripts/test_browser_companion.mjs   (also run by
// integration::browser_assets::tests when node is on PATH)
import { Companion } from '../src/integration/assets/browser/activity.mjs';

let fetches = 0;
const worker = { type: 'service_worker', url: 'chrome-extension://abc/sw.js', webSocketDebuggerUrl: 'ws://fake/sw' };
// The first call's lookups (one probe plus the nudge retries) all miss; later ones find the worker.
globalThis.fetch = async () => ({ json: async () => (fetches++ < 20 ? [] : [worker]) });
class FakeSocket {
  constructor() { this.readyState = 0; setTimeout(() => { this.readyState = 1; this.onopen && this.onopen(); }, 0); }
  send(text) {
    const { id, params } = JSON.parse(text);
    const value = params.expression.startsWith('herdrPing') ? 'herdr-companion/9' : { ok: true };
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
if (pong !== 'herdr-companion/9') { console.error(`FAIL: second call did not reach the worker: ${pong}`); process.exit(1); }
if (fetches < 21) { console.error('FAIL: the second call did not look for the worker again'); process.exit(1); }
console.log('ok: a transient worker miss fails one call and the next one reconnects');
