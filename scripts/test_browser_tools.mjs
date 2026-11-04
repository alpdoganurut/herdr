// herdr browser tools: what an act reports and how it fails, and what a read
// shows of an open dialog.
// Part 1 (no browser): the act's change report (actChanges), the Playwright
// call-log tail and the action_timeout message.
// Part 2 (a headless Chromium, when playwright-core and a browser binary are
// found; otherwise skipped with exit 0): the extraction puts an open dialog /
// in-page modal first as a `## Dialog` block even when the rest is <main>
// only, and drops closed ones; the sidecar (host.mjs over its JSON-lines pipe,
// attached over CDP to a headless shell this script starts) reports the modal
// a click opened, answers action_timeout (not stale_ref) when a resolved
// element never becomes clickable and stale_ref when it left the page, and
// logs the timeout's whole message to its stderr (host.log).
// Run: node scripts/test_browser_tools.mjs   (part 1 also runs from
// integration::browser_assets::tests when node is on PATH)
//   HERDR_TEST_PLAYWRIGHT_CORE=<dir of playwright-core>  (default: apps/companion's)
//   HERDR_TEST_CHROMIUM=<browser binary>  (default: playwright's, else its newest headless shell)
// Never touches herdr's own browser, profiles or state.
import { readFileSync, existsSync, mkdtempSync, mkdirSync, cpSync, symlinkSync, readdirSync, rmSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import http from 'node:http';
import os from 'node:os';
import readline from 'node:readline';
import { EXTRACT_SOURCE, MODALS_SOURCE, actChanges, callLogTail, actTimeoutMessage, redactActText } from '../src/integration/assets/browser/extract.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..');
const assets = join(repo, 'src/integration/assets/browser');
// throws, so a failure in part 2 still stops the browser it started
function check(cond, msg) { if (!cond) throw new Error(msg); }

// ------------------------------------------------------------ part 1: pure
{
  const merge = { title: 'Merge this request?', buttons: ['Cancel', 'Merge'] };
  const before = { modals: [], title: 'MR !12', focused: { role: 'button', name: 'Merge' } };
  const after = { modals: [merge], title: 'MR !12 · confirm', focused: { role: 'button', name: 'Cancel' } };
  const c = actChanges(before, after);
  check(c.modal && c.modal.title === 'Merge this request?' && c.modal.buttons.join() === 'Cancel,Merge', 'a modal that opened is reported with its title and buttons');
  check(c.title_before === 'MR !12', 'a title change keeps the old title');
  check(c.focused && c.focused.name === 'Cancel', 'focus that moved is reported');
  check(Object.keys(actChanges(after, after)).length === 0, 'nothing changed: nothing reported (an already open modal is not new)');
  check(Object.keys(actChanges(null, null)).length === 0, 'a failed probe reports nothing');
  check(actChanges(null, after).modal && actChanges(null, after).title_before === undefined, 'without a before probe a modal still shows, no title change is claimed');
  const message = 'locator.click: Timeout 3000ms exceeded.\nCall log:\n  - waiting for locator(\'aria-ref=e5\')\n    - locator resolved to <button id="b">Go</button>\n  - attempting click action\n    - element is not visible\n  - retrying click action\n    - element is not visible\n';
  check(callLogTail(message) === 'attempting click action · element is not visible', `the call log tail is the last distinct steps: ${callLogTail(message)}`);
  // Playwright 1.63's own shape: dimmed lines, counted repeats, retry bookkeeping
  const real = 'locator.click: Timeout 2500ms exceeded.\nCall log:\n\u001b[2m  - waiting for locator(\'aria-ref=e6\')\u001b[22m\n\u001b[2m  - attempting click action\u001b[22m\n\u001b[2m    5 × waiting for element to be visible, enabled and stable\u001b[22m\n\u001b[2m      - element is not visible\u001b[22m\n\u001b[2m    - retrying click action\u001b[22m\n\u001b[2m      - waiting 500ms\u001b[22m\n';
  check(callLogTail(real) === '5 × waiting for element to be visible, enabled and stable · element is not visible', `the tail skips colours and retry noise: ${callLogTail(real)}`);
  check(callLogTail('no log here') === '', 'no call log: no tail');
  const text = actTimeoutMessage({ kind: 'click', target: 'e5', role: 'button', name: 'Go', timeout_ms: 3000, force: true, hidden: true, message });
  check(!text.includes('\n'), 'the timeout message is one line');
  check(text.includes('button "Go" (e5)') && text.includes('3000 ms') && text.includes('forced') && text.includes('playwright: attempting click action · element is not visible') && text.includes('browser_screenshot'), `the timeout message names the element, budget, mode, call log and the next step: ${text}`);
  check(!/stale/i.test(text), 'a timeout is not called stale');
  // a forced fill on a background tab: Playwright's call log quotes the text
  // and no waiting line follows, so it would be the tail
  const secret = 'hunter2 "pw"\nline2';
  const forced = `locator.fill: Timeout 3000ms exceeded.\nCall log:\n  - waiting for locator('aria-ref=e7')\n    - locator resolved to <input id="p">\n  - fill("${secret}")\n  - elementHandle.type("s3cr3t-typed")\n`;
  const filled = actTimeoutMessage({ kind: 'fill', target: 'e7', role: 'textbox', name: 'Password', timeout_ms: 3000, force: true, hidden: true, connected: true, message: redactActText(forced, secret) });
  check(!filled.includes('hunter2') && !filled.includes('s3cr3t') && filled.includes('fill("‹18 chars›")') && filled.includes('type("‹12 chars›")'), `the timeout message carries the text's length, never the text: ${filled}`);
  const logged = redactActText(forced, secret);
  check(!logged.includes('hunter2') && !logged.includes('line2') && !logged.includes('s3cr3t'), `the logged message masks the text: ${logged}`);
  check(!callLogTail(forced).includes('s3cr3t') && !callLogTail(forced).includes('hunter2'), `the tail masks a quoted text even unredacted: ${callLogTail(forced)}`);
  check(redactActText(redactActText(forced, secret)) === redactActText(forced, secret), 'masking twice changes nothing');
  check(actTimeoutMessage({ kind: 'click', target: 'button.go', timeout_ms: 3000, connected: false, message }).includes('left the page during the click'), 'a selector target that left the page says so');
  check(!actTimeoutMessage({ kind: 'click', target: 'e5', timeout_ms: 3000, connected: null, message }).includes('still resolves'), 'an unanswered probe claims nothing');
}
console.log('ok: act change report and timeout message');

// ------------------------------------------------------ part 2: a browser
// (the nextest harness runs part 1 only)
if (process.env.HERDR_BROWSER_TOOLS_NO_BROWSER === '1') { console.log('skip: the browser part (HERDR_BROWSER_TOOLS_NO_BROWSER=1)'); process.exit(0); }
const require = createRequire(import.meta.url);
const pwCandidates = [process.env.HERDR_TEST_PLAYWRIGHT_CORE, join(repo, 'apps/companion/node_modules/playwright-core'), join(repo, 'apps/companion/install/node_modules/playwright-core')].filter(Boolean);
const pwDir = pwCandidates.find((dir) => existsSync(join(dir, 'package.json')));
if (!pwDir) { console.log('skip: no playwright-core (set HERDR_TEST_PLAYWRIGHT_CORE)'); process.exit(0); }
const { chromium } = require(pwDir);
function browserBinary() {
  if (process.env.HERDR_TEST_CHROMIUM) return existsSync(process.env.HERDR_TEST_CHROMIUM) ? process.env.HERDR_TEST_CHROMIUM : null;
  try { const p = chromium.executablePath(); if (p && existsSync(p)) return p; } catch {}
  const cache = process.platform === 'darwin' ? join(os.homedir(), 'Library/Caches/ms-playwright') : join(os.homedir(), '.cache/ms-playwright');
  if (!existsSync(cache)) return null;
  const shells = readdirSync(cache).filter((d) => d.startsWith('chromium_headless_shell-')).sort((a, b) => Number(b.split('-')[1]) - Number(a.split('-')[1]));
  for (const d of shells) {
    for (const sub of ['chrome-mac', 'chrome-mac-arm64', 'chrome-linux', 'chrome-headless-shell-mac-arm64', 'chrome-headless-shell-linux64']) {
      for (const bin of ['headless_shell', 'chrome-headless-shell']) {
        const p = join(cache, d, sub, bin);
        if (existsSync(p)) return p;
      }
    }
  }
  return null;
}
const binary = browserBinary();
if (!binary) { console.log('skip: no Chromium binary (set HERDR_TEST_CHROMIUM)'); process.exit(0); }

const PAGE = `<!doctype html><html><head><title>MR !12</title></head><body>
<nav>Projects · Groups</nav>
<main><h1>Merge request !12</h1><p>${'Body text. '.repeat(30)}</p>
<button id="open">Merge</button> <button id="hidden" style="display:none">Hidden</button> <button id="vanish">Vanish</button>
<dialog id="closed"><p>closed dialog text</p></dialog></main>
<div id="modal" role="dialog" aria-modal="true" aria-labelledby="mt" style="display:none"><h2 id="mt">Merge this request?</h2><p>The source branch will be removed.</p><button>Cancel</button><button>Merge now</button></div>
<script>
document.getElementById('open').addEventListener('click', () => { document.getElementById('modal').style.display = 'block'; document.title = 'MR !12 · confirm'; });
</script></body></html>`;

// --- the extraction, in a page of Playwright's own browser
{
  const browser = await chromium.launch({ executablePath: binary, headless: true });
  try {
    const page = await browser.newPage();
    await page.setContent(PAGE.replace('style="display:none"><h2', '><h2'));
    const extractFn = new Function('args', 'return (' + EXTRACT_SOURCE + ')(args);');
    const r = await page.evaluate(extractFn, {});
    check(r.markdown.startsWith('## Dialog: Merge this request?'), `the open modal comes first, though the rest is <main>: ${r.markdown.slice(0, 120)}`);
    check(r.markdown.includes('[button: Merge now]') && r.markdown.includes('source branch will be removed'), 'the modal\'s text and buttons are in the block');
    check(r.markdown.split('source branch will be removed').length === 2 && r.full_markdown.split('source branch will be removed').length === 2, 'the dialog is not repeated in the body');
    check(!r.markdown.includes('closed dialog text') && !r.full_markdown.includes('closed dialog text'), 'a closed <dialog> stays out');
    check(r.markdown.includes('# Merge request !12') && !r.markdown.includes('Projects · Groups'), 'the main content follows, the nav is still dropped');
    check(r.full_markdown.startsWith('## Dialog: Merge this request?'), 'the whole-page copy leads with it too');
    check(r.dialogs === 1 && r.main_chars < r.markdown.length, 'main_chars counts the main content only');
    // an open <dialog> element (not skipped any more)
    await page.setContent('<main><p>main</p><dialog open><h3>Delete?</h3><button>Yes</button></dialog></main>');
    const d = await page.evaluate(extractFn, {});
    check(d.markdown.startsWith('## Dialog: Delete?') && d.markdown.includes('[button: Yes]'), `an open <dialog> is read: ${d.markdown}`);
    // nothing open: no block
    await page.setContent('<main><p>just text</p><div role="dialog" hidden>gone</div></main>');
    const n = await page.evaluate(extractFn, {});
    check(!n.markdown.includes('## Dialog') && !n.markdown.includes('gone'), 'a hidden dialog adds nothing');
    // the act probe sees only visible modal dialogs
    const modalsFn = new Function('return (' + MODALS_SOURCE + ')();');
    await page.setContent(PAGE);
    const closed = await page.evaluate(modalsFn);
    check(closed.modals.length === 0 && closed.title === 'MR !12', 'no modal before the click');
    await page.click('#open');
    const open = await page.evaluate(modalsFn);
    check(open.modals.length === 1 && open.modals[0].title === 'Merge this request?' && open.modals[0].buttons.join() === 'Cancel,Merge now', `the probe reads the modal: ${JSON.stringify(open)}`);
  } finally {
    await browser.close();
  }
}
console.log('ok: dialogs in the extraction and the modal probe');

// --- the sidecar's act against a headless shell this script starts
const tmp = mkdtempSync(join(os.tmpdir(), 'herdr-browser-tools-'));
const hostDir = join(tmp, 'host');
mkdirSync(join(hostDir, 'node_modules'), { recursive: true });
for (const f of ['host.mjs', 'activity.mjs', 'extract.mjs', 'package.json']) cpSync(join(assets, f), join(hostDir, f));
cpSync(join(assets, 'companion'), join(hostDir, 'companion'), { recursive: true });
symlinkSync(pwDir, join(hostDir, 'node_modules/playwright-core'), 'dir');

const server = http.createServer((req, res) => { res.setHeader('content-type', 'text/html; charset=utf-8'); res.end(PAGE); });
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const url = `http://127.0.0.1:${server.address().port}/mr`;

const chrome = spawn(binary, ['--headless', '--remote-debugging-port=0', `--user-data-dir=${join(tmp, 'profile')}`, '--no-first-run', '--no-default-browser-check', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
const cleanup = () => {
  try { chrome.kill('SIGKILL'); } catch {}
  server.close();
  try { rmSync(tmp, { recursive: true, force: true }); } catch {}
};
let failed = false;
try {
  const port = await new Promise((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(() => reject(new Error(`no DevTools port: ${buf.slice(-400)}`)), 15000);
    chrome.stderr.on('data', (chunk) => {
      buf += chunk;
      const m = buf.match(/DevTools listening on ws:\/\/[^:]+:(\d+)\//);
      if (m) { clearTimeout(timer); resolve(Number(m[1])); }
    });
    chrome.on('exit', (code) => reject(new Error(`the browser exited (${code}): ${buf.slice(-400)}`)));
  });

  let stderr = '';
  const host = spawn(process.execPath, [join(hostDir, 'host.mjs')], { cwd: hostDir, stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, NODE_NO_WARNINGS: '1' } });
  host.stderr.on('data', (chunk) => { stderr += chunk; });
  const pending = new Map();
  const logs = [];
  readline.createInterface({ input: host.stdout }).on('line', (line) => {
    let msg; try { msg = JSON.parse(line); } catch { return; }
    if (msg.event === 'log') logs.push(msg);
    if (msg.event) return;
    const done = pending.get(msg.id); pending.delete(msg.id);
    if (done) done(msg);
  });
  let id = 0;
  const call = (op, extra = {}) => new Promise((resolve) => {
    const req = { id: ++id, op, deadline_ms: 15000, ...extra };
    pending.set(req.id, resolve);
    host.stdin.write(JSON.stringify(req) + '\n');
  });
  try {
    const hello = await call('hello');
    check(hello.ok, `hello: ${JSON.stringify(hello)}`);
    const attached = await call('attach', { profile: 't', args: { port }, deadline_ms: 20000 });
    check(attached.ok, `attach: ${JSON.stringify(attached.error)}`);
    const opened = await call('open', { profile: 't', args: { url, background: true } });
    check(opened.ok, `open: ${JSON.stringify(opened.error)}`);
    const target = opened.result.target;
    const act = (args, deadline_ms = 4000) => call('act', { profile: 't', target, args: { allow_password: false, ...args }, deadline_ms });

    // refs first (an open aria-modal hides the rest of the page from the snapshot)
    const snap = await call('read', { profile: 't', target, args: { format: 'snapshot' } });
    check(snap.ok, `snapshot: ${JSON.stringify(snap.error)}`);
    const refOf = (name) => { const m = snap.result.content.match(new RegExp(`button "${name}" \\[ref=(e\\d+)\\]`)); return m && m[1]; };
    const vanish = refOf('Vanish');
    const merge = refOf('Merge');
    check(vanish && merge, `refs in the snapshot: ${snap.result.content.slice(-600)}`);

    // a click that opens an in-page modal reports it
    const clicked = await act({ kind: 'click', selector: '#open' });
    check(clicked.ok, `click: ${JSON.stringify(clicked.error)}`);
    check(clicked.result.modal && clicked.result.modal.title === 'Merge this request?' && clicked.result.modal.buttons.includes('Merge now'), `the click reports the modal: ${JSON.stringify(clicked.result)}`);
    check(clicked.result.title_before === 'MR !12' && clicked.page.title === 'MR !12 · confirm', `and the title change: ${JSON.stringify([clicked.result, clicked.page])}`);
    check(clicked.page.dialog_open === false, 'no JavaScript dialog: dialog_open stays false');
    const read = await call('read', { profile: 't', target, args: { format: 'markdown' } });
    check(read.ok && read.result.content.startsWith('## Dialog: Merge this request?'), `a read shows the modal first: ${read.ok ? read.result.content.slice(0, 80) : JSON.stringify(read.error)}`);

    // a ref that resolves but never becomes clickable: action_timeout
    const hide = await call('eval', { profile: 't', target, args: { expr: "document.getElementById('open').style.display = 'none'; const v = document.getElementById('vanish'); v.style.display = 'none'; setTimeout(() => v.remove(), 600); true" } });
    check(hide.ok, `eval: ${JSON.stringify(hide.error)}`);
    const timedOut = await act({ kind: 'click', ref: merge }, 3000);
    check(!timedOut.ok && timedOut.error.code === 'action_timeout', `a resolved ref that times out is action_timeout: ${JSON.stringify(timedOut.error)}`);
    check(/did not complete within \d+ ms/.test(timedOut.error.message) && /forced|not forced/.test(timedOut.error.message) && timedOut.error.message.includes('browser_screenshot'), `the message says what happened: ${timedOut.error.message}`);
    check(timedOut.error.message.includes("playwright: "), `with the call log tail: ${timedOut.error.message} / ${stderr.slice(-1500)}`);
    // the element left the page during the click: stale_ref
    const gone = await act({ kind: 'click', ref: vanish }, 3000);
    check(!gone.ok && gone.error.code === 'stale_ref', `an element that left the page is stale_ref: ${JSON.stringify(gone.error)}`);
    // a selector that resolved but times out is action_timeout too
    const hidden = await act({ kind: 'click', selector: '#hidden' }, 3000);
    check(!hidden.ok && hidden.error.code === 'action_timeout', `selector timeout: ${JSON.stringify(hidden.error)}`);
    // host.log (stderr) has the whole message, herdr's log the summary at info
    check(/act timeout .*"elapsed_ms":\d+/.test(stderr) && stderr.includes('Call log'), `host.log gets the full message: ${stderr.slice(-600)}`);
    check(logs.some((l) => l.level === 'info' && l.text.startsWith('act timeout') && l.text.includes('"force"')), 'an info log event with hidden/force and timings');
    await call('close', { profile: 't', target });
  } finally {
    host.stdin.end();
  }
} catch (err) {
  failed = true;
  console.error(`FAIL: ${err && err.stack || err}`);
} finally {
  cleanup();
}
if (failed) process.exit(1);
console.log('ok: the sidecar reports modals and tells timeouts from stale refs');
process.exit(0);
