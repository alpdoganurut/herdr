// Live screenshot check (fork): a tab is never left stretched to its
// document's height after a screenshot (innerHeight == scrollHeight, no
// scrolling). Chromium's clipped Page.captureScreenshot resizes the page's view
// and, when it settles, puts back the size it saw when the request arrived; a
// capture that arrives while another is still pending can read the stretched
// size and put that back (every time when the two come from different CDP
// sessions, measured on Chrome 156). The host did that: a full-page shot of a
// background tab that did not paint timed out in Playwright (its own session)
// but stayed pending in Chromium, the quiet retry got the picture, and the
// downscaled inline copy went out on the host's session beside the pending
// one. The host now sends every capture on its one session through the page's
// CaptureLane. A background tab that does not paint cannot be made headlessly,
// so the renderer is stalled with a busy loop: the first capture stays pending
// past the stall timeout, as on a background companion tab.
//
// Runs headless Chrome over raw CDP (no Playwright, no window; a page with no
// device-metrics override, as a companion view or a connectOverCDP tab):
//   node scripts/test_browser_screenshot_live.mjs
// HERDR_TEST_CHROME=<binary> picks the browser; skipped when none is found.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { CaptureLane, laneShot, shotParams, captureWithQuietRetry } from '../src/integration/assets/browser/activity.mjs';

function check(cond, msg) { if (!cond) { console.error(`FAIL: ${msg}`); cleanup(); process.exit(1); } }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const candidates = [
  process.env.HERDR_TEST_CHROME,
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
].filter(Boolean);
const binary = candidates.find((c) => fs.existsSync(c));
if (!binary) { console.log('skip: no Chrome found (set HERDR_TEST_CHROME)'); process.exit(0); }

const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'herdr-shot-live-'));
const chrome = spawn(binary, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--window-size=614,900',
  '--no-first-run', '--no-default-browser-check', '--disable-gpu', 'about:blank'], { stdio: 'ignore' });
function cleanup() {
  try { chrome.kill(); } catch {}
  try { fs.rmSync(profile, { recursive: true, force: true }); } catch {}
}
process.on('exit', cleanup);

let port = null;
for (let i = 0; i < 100 && !port; i++) {
  await sleep(100);
  try { port = fs.readFileSync(path.join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]; } catch {}
}
check(port, 'Chrome wrote its DevTools port');
let pageTarget = null;
for (let i = 0; i < 50 && !pageTarget; i++) {
  try { pageTarget = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === 'page'); } catch {}
  if (!pageTarget) await sleep(100);
}
check(pageTarget, 'a page target');

// One CDP session on the page (a WebSocket to its target).
async function connect() {
  const ws = new WebSocket(pageTarget.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let nextId = 0;
  const waiting = new Map();
  ws.onmessage = (msg) => {
    const data = JSON.parse(msg.data);
    const pending = waiting.get(data.id);
    if (!pending) return;
    waiting.delete(data.id);
    if (data.error) pending.reject(new Error(data.error.message)); else pending.resolve(data.result);
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++nextId;
    waiting.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
  return { ws, send };
}
const host = await connect();
const send = host.send;
const evaluate = async (expression) => (await send('Runtime.evaluate', { expression, returnByValue: true })).result.value;
const dims = () => evaluate('[innerWidth, innerHeight, document.documentElement.scrollHeight]');
const settle = async () => { for (let i = 0; i < 30; i++) { await sleep(100); await evaluate('1'); } };
const stall = (ms) => { send('Runtime.evaluate', { expression: `{ const t = Date.now() + ${ms}; while (Date.now() < t); }` }).catch(() => {}); };
const fullClip = async () => {
  const m = await send('Page.getLayoutMetrics');
  const c = m.cssContentSize || m.contentSize;
  return { x: 0, y: 0, width: c.width, height: c.height };
};

await send('Page.enable');
const tall = 'data:text/html,' + encodeURIComponent('<body style="margin:0"><div style="height:6000px;background:linear-gradient(red,blue)">tall</div></body>');
await send('Page.navigate', { url: tall });
await sleep(500);
const [width, height, scrollHeight] = await dims();
check(scrollHeight === 6000 && height < scrollHeight, `a tall page in a short window: ${width}x${height} of ${scrollHeight}`);
const intact = async (what) => {
  await settle();
  const now = await dims();
  check(now[0] === width && now[1] === height, `${what}: the viewport is ${now[0]}x${now[1]}, was ${width}x${height} (document ${now[2]})`);
};

// 1. A full-page and a viewport shot that succeed leave the page as it was.
const lane = new CaptureLane();
const full = laneShot(lane, async () => send('Page.captureScreenshot', shotParams({ type: 'jpeg', quality: 70, clip: await fullClip(), beyond: true })));
check((await full.wait(10000)).data.length > 0, 'a full-page capture');
const view = laneShot(lane, () => send('Page.captureScreenshot', shotParams({ type: 'png', quality: 70 })));
check((await view.wait(10000)).data.length > 0, 'a viewport capture');
await intact('after a successful full-page and viewport shot');
console.log('ok: successful full-page and viewport shots leave the viewport as it was');

// 2. The fix: the stalled first capture times out, a quiet select, and the
//    retry waits on the same request; the inline copy (a scaled full page)
//    queued while it is pending waits for it.
const clip = await fullClip();
stall(2500);
await sleep(50);
const stalled = laneShot(lane, () => send('Page.captureScreenshot', shotParams({ type: 'jpeg', quality: 70, clip, beyond: true })));
const calls = [];
const retried = captureWithQuietRetry({
  capture: (ms) => { calls.push('capture'); return stalled.wait(ms); },
  select: async () => { calls.push('select'); return true; },
  stallMs: 2000,
  deadlineAt: Date.now() + 20000,
});
await sleep(900);
const inline = laneShot(lane, () => send('Page.captureScreenshot', shotParams({ type: 'jpeg', quality: 70, clip: { ...clip, scale: 0.25 }, beyond: true })));
const small = inline.wait(10000);
const shot = await retried;
check(shot.data.length > 0 && calls.join(',') === 'capture,select,capture', `the retry got the stalled capture: ${calls}`);
check((await small).data.length > 0, 'the inline copy');
await intact('after a stalled full-page capture, its retry and the inline copy');
console.log('ok: a stalled full-page capture, its quiet retry and the inline copy leave the viewport as it was');

// 3. The old host, for the record: the first capture on one session (Playwright's)
//    still pending, the next on another (the host's): the tab is left
//    stretched; a reload brings it back (the way to un-stick a tab).
const other = await connect();
stall(2500);
await sleep(50);
const first = other.send('Page.captureScreenshot', shotParams({ type: 'jpeg', quality: 70, clip, beyond: true })).catch((e) => e);
await sleep(800);
const second = send('Page.captureScreenshot', shotParams({ type: 'jpeg', quality: 70, clip: { ...clip, scale: 0.25 }, beyond: true })).catch((e) => e);
await Promise.all([first, second]);
await settle();
const broken = await dims();
check(broken[1] !== height, `two sessions side by side stretch the tab (the bug this guards): ${broken}`);
await send('Page.reload');
await sleep(800);
await intact('after a reload of the stretched tab');
console.log(`ok: the old host's two sessions side by side stretch the tab to ${broken[0]}x${broken[1]}; a reload brings it back`);
other.ws.close();
host.ws.close();
cleanup();
process.exit(0);
