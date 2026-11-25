// Companion browser host (fork): the sidecar's control-page driver and the
// HTML view watches, against a fake control page (no Electron, no
// Chromium): the nonce check, view calls and their error codes, the
// target -> view map, focus and raise against old and current companions,
// furniture URLs, the view watches (only the page and the files it
// references reload it, a half-written, growing or mid-rename page waits for
// its complete file, one reload at a time, the watch cap), the companion's page
// dialogs (a Playwright-like Dialog over the control page, push reconcile).
// The remote attach (`remote: true`): its page allowlist, the input queue and,
// through host.mjs over its JSON-lines pipe with a fake playwright-core (no
// browser), what it changes and that a local attach stays as it was.
// Run: node scripts/test_browser_companion_host.mjs   (also run by
// integration::browser_assets::tests when node is on PATH)
import { ControlPage, ViewWatches, refsInside, reloadKeepingState, captureViewState, restoreViewState, isFurnitureUrl, CONTROL_URL_PREFIX, CompanionDialog, reconcileDialogs, pruneAnswered, isRemotePageUrl, isRemoteTabUrl, remoteRefusal, isRemoteFile, InputLane, releaseModifiers } from '../src/integration/assets/browser/activity.mjs';
import { spawn } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { join } from 'node:path';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';

function check(cond, msg) { if (!cond) { console.error(`FAIL: ${msg}`); process.exit(1); } }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A fake control page: `window.__herdrCompanion` answers in-process.
const calls = [];
const api = {
  nonce: () => 'n-1',
  views: () => [{ view_id: 'v7', target_id: 'T7', tab_id: 'w1:t1' }],
  select: (view) => (view === 'v7' ? {} : { error: { code: 'view_not_found', message: 'no view' } }),
  open: (arg) => ({ view_id: 'v8', target_id: 'T8', tab_id: arg.tab_id }),
};
const control = { url: () => CONTROL_URL_PREFIX, isClosed: () => false };
const strip = { url: () => 'herdr-companion://ui/strip.html', isClosed: () => false };
const page = { url: () => 'https://example.test/', isClosed: () => false };
const driver = new ControlPage({
  pages: () => [page, strip, control],
  evaluate: async (target, fn, arg) => {
    calls.push(arg[0]);
    check(target === control, 'calls go to the control page only');
    globalThis.__herdrCompanion = api;
    try { return fn(arg); } finally { delete globalThis.__herdrCompanion; }
  },
  callMs: 500,
});

check(await driver.verify('n-1'), 'the right nonce verifies');
let failed = null;
try { await driver.verify('other'); } catch (err) { failed = err; }
check(failed && failed.code === 'host_verify_failed' && failed.herdr, `a wrong nonce is host_verify_failed (${failed && failed.code})`);
console.log('ok: the control page answers the registration nonce');

check(await driver.viewOf('T7') === 'v7', 'an unknown target is looked up through views()');
check(calls.filter((c) => c === 'views').length === 1, 'views() asked once');
await driver.viewOf('T7');
check(calls.filter((c) => c === 'views').length === 1, 'then remembered');
failed = null;
try { await driver.viewOf('T-gone'); } catch (err) { failed = err; }
check(failed && failed.code === 'tab_closed', 'a target the companion does not know is tab_closed');
failed = null;
try { await driver.call('select', 'v-missing'); } catch (err) { failed = err; }
check(failed && failed.code === 'view_not_found', 'page errors keep their code');
const opened = await driver.call('open', { tab_id: 'w1:t3', url: 'about:blank' });
check(opened.view_id === 'v8' && opened.tab_id === 'w1:t3', 'open answers the view');
failed = null;
try { await driver.call('missing'); } catch (err) { failed = err; }
check(failed && failed.code === 'host_verify_failed', 'an API without the function is host_verify_failed');
console.log('ok: view calls map targets to views and keep error codes');

// Fork (companion launcher, audit 3): focus and raise against companions old and new. A
// companion from before the user flag reads focus's argument as a view id only and has no
// raise(); an agent's focus goes bare to both, the user's own falls back to it.
function companionPage(api) {
  const sent = [];
  const driver = new ControlPage({
    pages: () => [control],
    evaluate: async (_target, fn, arg) => {
      sent.push(arg);
      globalThis.__herdrCompanion = api;
      try { return fn(arg); } finally { delete globalThis.__herdrCompanion; }
    },
    callMs: 500,
  });
  return { driver, sent };
}
const known = (id) => id === 'v7';
const oldApi = {
  focus: (view) => (known(view) ? { ok: true, view_id: view } : { error: { code: 'view_not_found', message: `no view ${view}` } }),
  views: () => [{ view_id: 'v6', shown: false, active: true }, { view_id: 'v7', shown: true, active: true }],
};
const newApi = {
  focus: (arg) => {
    const view = arg && typeof arg === 'object' ? arg.view_id : arg;
    return known(view) ? { ok: true, view_id: view, user: Boolean(arg && arg.user) } : { error: { code: 'view_not_found', message: `no view ${view}` } };
  },
  raise: (arg) => ({ ok: true, raised: Boolean(arg && arg.user) }),
  views: oldApi.views,
};
let page1 = companionPage(oldApi);
await page1.driver.focusView('v7', false);
check(JSON.stringify(page1.sent) === JSON.stringify([['focus', 'v7']]), `an agent's focus is the bare view id (${JSON.stringify(page1.sent)})`);
const userOld = await page1.driver.focusView('v7', true);
check(userOld.ok && JSON.stringify(page1.sent.slice(1)) === JSON.stringify([['focus', { view_id: 'v7', user: true }], ['focus', 'v7']]), `the user's own falls back to the bare id on an old companion (${JSON.stringify(page1.sent)})`);
failed = null;
try { await page1.driver.focusView('v-gone', true); } catch (err) { failed = err; }
check(failed && failed.code === 'view_not_found', 'a view that is gone stays view_not_found');
const raisedOld = await page1.driver.raise(true);
check(raisedOld.ok && JSON.stringify(page1.sent.slice(-3)) === JSON.stringify([['raise', { user: true }], ['views', null], ['focus', 'v7']]), `the user's raise on an old companion focuses its shown page (${JSON.stringify(page1.sent.slice(-3))})`);
failed = null;
try { await page1.driver.raise(false); } catch (err) { failed = err; }
check(failed && failed.code === 'companion_outdated' && /quit it/.test(failed.message), `an agent's raise there says why (${failed && failed.code})`);
let page2 = companionPage(newApi);
const userNew = await page2.driver.focusView('v7', true);
check(userNew.user === true && page2.sent.length === 1, 'a current companion takes the user flag at once');
const agentNew = await page2.driver.focusView('v7', false);
check(agentNew.ok && agentNew.user === false, "and reads the bare id as an agent's focus");
const raisedNew = await page2.driver.raise(true);
check(raisedNew.raised === true && page2.sent.at(-1)[0] === 'raise', 'a current companion raises itself');
failed = null;
const noApi = new ControlPage({ pages: () => [control], evaluate: async (_t, fn, arg) => fn(arg), callMs: 500 });
try { await noApi.raise(true); } catch (err) { failed = err; }
check(failed && failed.code === 'host_verify_failed' && !failed.missing, 'a page without the companion API is not an old companion');
console.log('ok: focus and raise reach old and current companions');

// A control page that never answers times out.
const hung = new ControlPage({ pages: () => [control], evaluate: () => new Promise(() => {}), callMs: 50 });
failed = null;
try { await hung.call('nonce'); } catch (err) { failed = err; }
check(failed && failed.code === 'browser_timeout', 'a hung control page is browser_timeout');
const none = new ControlPage({ pages: () => [page], evaluate: async () => null });
failed = null;
try { await none.call('nonce'); } catch (err) { failed = err; }
check(failed && failed.code === 'host_verify_failed', 'no control page is host_verify_failed');
console.log('ok: a hung or missing control page fails fast');

check(isFurnitureUrl('herdr-companion://control/') && isFurnitureUrl('herdr-companion://ui/strip.html'), 'the companion pages are furniture');
check(!isFurnitureUrl('https://example.test/') && !isFurnitureUrl('about:blank#herdr-view=v1') && !isFurnitureUrl(undefined), 'other pages are not');
console.log('ok: furniture URLs');

// Watches: file and directory, debounced into one reload, capped, disarmed.
const watchers = [];
const watchFn = (path, cb) => { const w = { path, cb, closed: false, close() { this.closed = true; } }; watchers.push(w); return w; };
// an in-memory file system: path -> { text, mtimeMs }
const disk = new Map();
let clock = 1;
const put = (path, text) => disk.set(path, { text, mtimeMs: clock++ });
const statFn = async (path) => { const f = disk.get(path); if (!f) throw Object.assign(new Error('ENOENT'), { code: 'ENOENT' }); return { size: f.text.length, mtimeMs: f.mtimeMs }; };
let onRead = null; // a one-shot hook: runs inside the check's read of the page, before it answers
const readFn = async (path) => { const f = disk.get(path); if (!f) throw new Error('ENOENT'); const text = f.text; if (onRead) { const hook = onRead; onRead = null; hook(); } return text; };
const timing = { debounceMs: 20, stableMs: 60, pollMs: 15, maxWaitMs: 500 };
const watches = new ViewWatches({ watchFn, statFn, readFn, ...timing, max: 2 });
const PAGE = '/tmp/views/report.html';
const DOC = (body) => `<!doctype html><html><body>${body}</body></html>\n`;
put(PAGE, DOC('one'));
const reloads = []; // the page text each reload saw
let refsAfterReload;
const reloadFn = async () => { reloads.push((disk.get(PAGE) || {}).text); return refsAfterReload; };
check(watches.arm('T1', PAGE, reloadFn), 'armed');
check(watchers.map((w) => w.path).join(',') === '/tmp/views/report.html,/tmp/views', 'the file and its directory are watched');
await sleep(20); // the armed page's shape is read in the background
const fileW = watchers[0];
const dirW = watchers[1];
put(PAGE, DOC('two'));
for (let i = 0; i < 5; i++) (i % 2 ? dirW.cb('change', 'report.html') : fileW.cb('change', 'report.html'));
await sleep(200);
check(reloads.length === 1, `a burst of changes (file and directory watchers) is one reload (${reloads.length})`);
fileW.cb('change', 'report.html'); dirW.cb('change', 'report.html');
await sleep(200);
check(reloads.length === 1, `a late event for a change already loaded reloads nothing (${reloads.length})`);

// a side file next to the page (an image an agent generates) does not reload it
reloads.length = 0;
put('/tmp/views/chart.png', 'png');
dirW.cb('rename', 'chart.png'); dirW.cb('change', 'chart.png'); dirW.cb('change', 'report.html.swp');
await sleep(200);
check(reloads.length === 0, `a side file in the page's directory does not reload it (${reloads.length})`);

// a referenced file does; one in a subdirectory gets that directory watched; a side file there does not
check(watches.setRefs('T1', ['/tmp/views/chart.png', '/tmp/views/img/logo.svg', '/tmp/views/img/2x/logo.svg', 'relative.png', PAGE]), 'refs set');
check(watches.watched('T1').join(',') === '/tmp/views,/tmp/views/img,/tmp/views/img/2x', `the referenced subdirectories are watched (${watches.watched('T1')})`);
dirW.cb('change', 'chart.png');
await sleep(200);
check(reloads.length === 1, `a referenced image next to the page reloads it (${reloads.length})`);
const imgW = watchers.find((w) => w.path === '/tmp/views/img' && !w.closed);
put('/tmp/views/img/other.png', 'x');
imgW.cb('rename', 'other.png');
await sleep(200);
check(reloads.length === 1, `a side file in a referenced subdirectory does not (${reloads.length})`);
put('/tmp/views/img/logo.svg', '<svg/>');
imgW.cb('change', 'logo.svg');
await sleep(200);
check(reloads.length === 2, `a referenced file in the subdirectory does (${reloads.length})`);
// the refs a reload answers replace the old ones: the subdirectory goes, its watcher closes
refsAfterReload = ['/tmp/views/style.css'];
put(PAGE, DOC('three'));
fileW.cb('change', 'report.html');
await sleep(200);
check(reloads.length === 3 && watches.watched('T1').join(',') === '/tmp/views' && imgW.closed, `a reload's refs replace the old ones (${watches.watched('T1')})`);
reloads.length = 0;
dirW.cb('change', 'chart.png');
await sleep(150);
check(reloads.length === 0, 'a file no longer referenced does not reload it');
put('/tmp/views/style.css', 'body{}');
dirW.cb('change', 'style.css');
await sleep(200);
check(reloads.length === 1, 'the newly referenced stylesheet does');
// the extra directories are capped
const capped = new ViewWatches({ watchFn: () => ({ close() {} }), maxDirs: 2 });
capped.arm('C', '/a/p.html', () => {});
capped.setRefs('C', ['/b/1.png', '/c/2.png', '/d/3.png']);
check(capped.watched('C').join(',') === '/a,/b,/c', `at most maxDirs extra directories (${capped.watched('C')})`);
capped.disarm('C');
console.log('ok: only the page and the files it references reload it');

// a chunked write: no reload until the page ends with </html> again, then one of the final text
reloads.length = 0;
put(PAGE, '<!doctype html><html><body>four, part');
fileW.cb('change', 'report.html');
await sleep(150);
put(PAGE, '<!doctype html><html><body>four, part one, part');
fileW.cb('change', 'report.html');
await sleep(150);
check(reloads.length === 0, `a half-written page (no </html> yet) is not loaded (${reloads.length})`);
put(PAGE, DOC('four, part one, part two'));
fileW.cb('change', 'report.html');
await sleep(200);
check(reloads.length === 1 && reloads[0] === DOC('four, part one, part two'), `the finished file is loaded once (${JSON.stringify(reloads)})`);
// a file whose size keeps moving is waited for; then one reload
reloads.length = 0;
for (let i = 0; i < 6; i++) { put(PAGE, DOC('five ' + 'x'.repeat(i))); fileW.cb('change', 'report.html'); await sleep(35); }
check(reloads.length === 0, `a file still growing is not loaded (${reloads.length})`);
await sleep(200);
check(reloads.length === 1 && reloads[0] === DOC('five xxxxx'), `then once, its last text (${JSON.stringify(reloads)})`);
// a page that never ends with </html> again loads after maxWaitMs anyway
reloads.length = 0;
put(PAGE, '<p>no closing tag');
fileW.cb('change', 'report.html');
await sleep(300);
check(reloads.length === 0, 'an unfinished page waits');
await sleep(400);
check(reloads.length === 1, `and is loaded after maxWaitMs (${reloads.length})`);
// a page that never ended with </html> does not wait for one
reloads.length = 0;
put(PAGE, '<p>still no closing tag, edited');
fileW.cb('change', 'report.html');
await sleep(200);
check(reloads.length === 1, `a page without </html> before is loaded once stable (${reloads.length})`);
put(PAGE, DOC('six'));
fileW.cb('change', 'report.html');
await sleep(200);
console.log('ok: a half-written or growing page waits for its complete file');

// an atomic save: the temp file (a side file) is written, then renamed over the page; the
// page is missing in between
reloads.length = 0;
put('/tmp/views/.report.html.tmp', DOC('seven'));
dirW.cb('rename', '.report.html.tmp'); dirW.cb('change', '.report.html.tmp');
disk.delete(PAGE);
fileW.cb('rename', 'report.html'); dirW.cb('rename', 'report.html');
await sleep(150);
check(reloads.length === 0, `a page missing mid-save is not loaded (${reloads.length})`);
disk.set(PAGE, disk.get('/tmp/views/.report.html.tmp')); disk.delete('/tmp/views/.report.html.tmp');
dirW.cb('rename', '.report.html.tmp'); dirW.cb('rename', 'report.html');
await sleep(200);
check(reloads.length === 1 && reloads[0] === DOC('seven'), `an atomic rename save reloads once, the new text (${JSON.stringify(reloads)})`);
// an event without a file name reloads only when the page changed since its last load
reloads.length = 0;
dirW.cb('change', null);
await sleep(200);
check(reloads.length === 0, 'a nameless event with the page unchanged does not reload');
put(PAGE, DOC('eight'));
dirW.cb('change', null);
await sleep(200);
check(reloads.length === 1, `a nameless event after the page changed does (${reloads.length})`);
console.log('ok: atomic saves and nameless events');

// a write whose event comes in while a check is busy (here: reading the page) is never dropped,
// even when that check finds nothing new to load
reloads.length = 0;
fileW.cb('change', 'report.html'); // a late event: the page is as loaded
onRead = () => { put(PAGE, DOC('nine, written mid-check')); fileW.cb('change', 'report.html'); };
await sleep(300);
check(onRead === null && reloads.length === 1 && reloads[0] === DOC('nine, written mid-check'), `a write during a check's awaits still reloads (${JSON.stringify(reloads)})`);
// the page file still gone at maxWaitMs: no reload over it; its return reloads
reloads.length = 0;
disk.delete(PAGE);
fileW.cb('rename', 'report.html');
await sleep(700);
check(reloads.length === 0, `a page still missing at maxWaitMs is not loaded (${reloads.length})`);
put(PAGE, DOC('ten, back'));
dirW.cb('rename', 'report.html');
await sleep(200);
check(reloads.length === 1 && reloads[0] === DOC('ten, back'), `its return reloads it (${JSON.stringify(reloads)})`);
console.log('ok: no lost events, no reload over a missing page');

// the referenced files a view may watch stay inside its root (real paths), kept as written
{
  const links = new Map([['/tmp', '/private/tmp']]); // a symlinked prefix, as macOS /tmp
  const files = new Set(['/private/tmp/views/a.png', '/private/tmp/views/img/b.png', '/private/tmp/outside.css', '/private/tmp/views/link.css', '/etc/passwd']);
  const realpath = (p) => {
    let r = p;
    for (const [from, to] of links) if (r === from || r.startsWith(from + '/')) r = to + r.slice(from.length);
    r = r.split('/').reduce((acc, part) => (part === '..' ? acc.slice(0, -1) : part === '.' || part === '' ? acc : [...acc, part]), []).join('/');
    r = '/' + r;
    if (r === '/private/tmp/views/link.css') return '/private/tmp/outside.css'; // a symlink out of the root
    if (files.has(r) || ['/private/tmp', '/private/tmp/views', '/private/tmp/views/img', '/private/tmp/views/new'].includes(r)) return r;
    throw new Error('ENOENT');
  };
  const kept = refsInside([
    '/tmp/views/a.png', '/tmp/views/img/b.png', '/tmp/views/new/c.png',
    '/tmp/views/../outside.css', '/private/tmp/outside.css', '/etc/passwd', '/tmp/views/link.css',
    '\\\\server\\share\\x.png', '//server/share/x.png', '/nowhere/x.png', 'relative.png',
  ], '/private/tmp/views', realpath);
  check(kept.join(',') === '/tmp/views/a.png,/tmp/views/img/b.png,/tmp/views/new/c.png', `only references inside the root stay, as written (${kept})`);
}
console.log('ok: references stay inside the view root');

// reloadKeepingState: a 'manual' scroll restoration left by a capture that timed out, or by a
// restore that failed, is set back, and the next capture is told it was ours
{
  const fakePage = ({ capture, restore, resetOk = true }) => {
    const calls = [];
    return {
      calls,
      async reload() { calls.push(['reload']); },
      evaluate(fn, arg) {
        if (fn === captureViewState) { calls.push(['capture', arg]); return capture(); }
        if (fn === restoreViewState) { calls.push(['restore']); return restore(); }
        calls.push(['reset']);
        return resetOk ? Promise.resolve() : Promise.reject(new Error('gone'));
      },
    };
  };
  const hung = fakePage({ capture: () => new Promise(() => {}), restore: async () => ({}) });
  const r1 = await reloadKeepingState(hung, { captureMs: 30 });
  check(r1.report === null && r1.ours === false && hung.calls.map((c) => c[0]).join() === 'capture,reload,reset', `a capture that timed out: reloaded, then set back (${JSON.stringify(hung.calls)}, ${JSON.stringify(r1)})`);
  const stuck = fakePage({ capture: () => new Promise(() => {}), restore: async () => ({}), resetOk: false });
  const r2 = await reloadKeepingState(stuck, { captureMs: 30 });
  check(r2.ours === true, 'when it cannot be set back, it stays ours');
  const next = fakePage({ capture: async () => ({ setManual: true }), restore: async () => ({ manual_left: false }) });
  const r3 = await reloadKeepingState(next, { captureMs: 30, ours: r2.ours });
  check(next.calls[0][0] === 'capture' && next.calls[0][1] === true && r3.ours === false, `the next capture is told the 'manual' is ours (${JSON.stringify(next.calls)})`);
  const failing = fakePage({ capture: async () => ({ setManual: true }), restore: async () => { throw new Error('navigated'); } });
  const r4 = await reloadKeepingState(failing, { captureMs: 30 });
  check(failing.calls.map((c) => c[0]).join() === 'capture,reload,restore,reset' && r4.ours === false, `a restore that failed sets it back (${JSON.stringify(failing.calls)})`);
}
console.log('ok: a scroll restoration of ours never sticks');

// one reload at a time: changes during a slow reload start one more after it
let running = 0; let overlap = false; let slow = 0;
const slowReload = async () => { running++; if (running > 1) overlap = true; slow++; await sleep(120); running--; };
watches.arm('T1', PAGE, slowReload);
const fileW2 = watchers.at(-2);
await sleep(20);
put(PAGE, DOC('nine')); fileW2.cb('change', 'report.html');
await sleep(110);
put(PAGE, DOC('ten')); fileW2.cb('change', 'report.html');
await sleep(40);
put(PAGE, DOC('eleven')); fileW2.cb('change', 'report.html');
await sleep(500);
check(!overlap && slow === 2, `changes during a reload make one more, never two at once (${slow}, overlap ${overlap})`);

const seen = watchers.length;
watches.arm('T2', '/tmp/views/b.html', () => {});
failed = null;
try { watches.arm('T3', '/tmp/views/c.html', () => {}); } catch (err) { failed = err; }
check(failed && failed.code === 'too_many_watches', 'the cap holds');
check(watches.arm('T1', PAGE, reloadFn), 're-arming a watched view is not over the cap');
check(watchers.slice(seen - 2, seen).every((w) => w.closed), 're-arming closes the old watchers');
check(watches.disarm('T1') && !watches.has('T1') && !watches.disarm('T1'), 'disarm once');
reloads.length = 0;
fileW.cb('change', 'report.html');
await sleep(120);
check(reloads.length === 0, 'a disarmed view never reloads');
watches.disarm('T2');
console.log('ok: html view watches debounce, cap and disarm');

// Fork (companion dialogs): alert / confirm / prompt live in the companion; the sidecar's
// stand-in for Playwright's Dialog answers through the control page.
const answers = [];
const dialogControl = { call: async (fn, arg) => { answers.push([fn, arg]); if (arg.view_id === 'gone') throw Object.assign(new Error('no dialog'), { herdr: true, code: 'no_dialog' }); return { ok: true }; } };
const answered = [];
const dlg = new CompanionDialog(dialogControl, { id: 'd3', view_id: 'v7', target_id: 'T7', type: 'prompt', message: 'Copy this:', default_value: 'abc' }, (id) => answered.push(id));
check(dlg.companion && dlg.type() === 'prompt' && dlg.message() === 'Copy this:' && dlg.defaultValue() === 'abc', 'the dialog reads like Playwright\'s');
await dlg.accept('x');
await dlg.accept(undefined);
await dlg.dismiss();
check(JSON.stringify(answers) === JSON.stringify([['dialog', { view_id: 'v7', id: 'd3', accept: true, text: 'x' }], ['dialog', { view_id: 'v7', id: 'd3', accept: true }], ['dialog', { view_id: 'v7', id: 'd3', accept: false }]]), `accept(text) / accept() / dismiss() answer through dialog() (${JSON.stringify(answers)})`);
check(answered.join() === 'd3,d3,d3', 'each answer is remembered before it goes');
failed = null;
try { await new CompanionDialog(dialogControl, { id: 'd4', view_id: 'gone', type: 'alert' }).accept(); } catch (err) { failed = err; }
check(failed && failed.code === 'no_dialog', 'an answer the companion refuses keeps its code');
check(new CompanionDialog(dialogControl, { id: 'd5', view_id: 'v', type: 'print' }).type() === 'alert', 'an unknown type reads as alert');
const pending = new Set();
const slowControl = { call: async () => { throw Object.assign(new Error('companion dialog did not answer in 5000 ms'), { herdr: true, code: 'browser_timeout' }); } };
failed = null;
try { await new CompanionDialog(slowControl, { id: 'd8', view_id: 'v8', type: 'prompt' }, (id) => pending.add(id), (id) => pending.delete(id)).accept('x'); } catch (err) { failed = err; }
check(failed && failed.code === 'browser_timeout' && !pending.has('d8'), 'an answer the companion did not take is forgotten again (keeps its code)');
const goneControl = { call: async () => { throw Object.assign(new Error('no dialog'), { herdr: true, code: 'no_dialog' }); } };
try { await new CompanionDialog(goneControl, { id: 'd10', view_id: 'v', type: 'alert' }, (id) => pending.add(id), (id) => pending.delete(id)).accept(); } catch {}
check(pending.has('d10'), 'a no_dialog answer stays answered (the dialog is gone)');
await new CompanionDialog(dialogControl, { id: 'd11', view_id: 'v', type: 'prompt' }).accept('');
check(JSON.stringify(answers.at(-1)) === JSON.stringify(['dialog', { view_id: 'v', id: 'd11', accept: true, text: '' }]), `accept("") sends an empty answer (${JSON.stringify(answers.at(-1))})`);
console.log('ok: companion dialogs answer through the control page');

const have = new CompanionDialog(dialogControl, { id: 'd1', view_id: 'v1', type: 'alert' });
let rec = reconcileDialogs(new Map([['T1', null], ['T2', have], ['T3', null]]), [{ id: 'd2', view_id: 'v3', target_id: 'T3', type: 'prompt' }, { id: 'd9', view_id: 'v9', target_id: 'T9', type: 'alert' }]);
check(rec.open.length === 1 && rec.open[0][0] === 'T3' && rec.close.join() === 'T2', `a push opens the new one, closes the gone one, ignores untracked pages (${JSON.stringify(rec)})`);
rec = reconcileDialogs(new Map([['T2', have]]), [{ id: 'd1', view_id: 'v1', target_id: 'T2', type: 'alert' }]);
check(!rec.open.length && !rec.close.length, 'the same dialog again changes nothing');
rec = reconcileDialogs(new Map([['T2', have]]), [{ id: 'd6', view_id: 'v1', target_id: 'T2', type: 'confirm' }]);
check(rec.open.length === 1 && rec.open[0][1].id === 'd6', 'the next dialog of the page replaces it');
rec = reconcileDialogs(new Map([['T2', null]]), [{ id: 'd1', view_id: 'v1', target_id: 'T2', type: 'alert' }], new Set(['d1']));
check(!rec.open.length, 'a push from before an answer does not bring the dialog back');
const ans = new Set(['d1', 'd2', 'd3']);
pruneAnswered(ans, [{ id: 'd2', target_id: 'T2' }]);
check([...ans].join() === 'd2', `the answered ids shrink to the pushed ones (${[...ans]})`);
pruneAnswered(ans, []);
check(ans.size === 0, 'an empty push forgets every answer');
console.log('ok: companion dialog pushes reconcile per page');

// ---------------------------------------------------------------------------
// Fork (remote browser): the pages of a remote attach
for (const url of ['http://localhost:3000/', 'https://example.test/a?b#c', 'about:blank', 'about:blank#herdr-view=v1', 'about:blank?x', 'herdr-file://abcdefghijklmnopqrstuvwx/index.html']) {
  check(isRemotePageUrl(url) && isRemoteTabUrl(url) && remoteRefusal('act', url) === null, `${url} may be driven`);
}
for (const url of ['devtools://devtools/bundled/devtools_app.html', 'herdr-companion://control/', 'herdr-companion://ui/strip.html', 'file:///etc/passwd', 'chrome://settings/', 'about:srcdoc', 'about:blankets', 'data:text/html,x', 'blob:https://example.test/1', 'view-source:https://example.test/', 'javascript:1', ' https://example.test/', '', undefined, null]) {
  check(!isRemotePageUrl(url) && !isRemoteTabUrl(url) && remoteRefusal('navigate', url), `${url} may not`);
}
check(!remoteRefusal('read', 'file:///Users/me/secret.txt').includes('secret') && remoteRefusal('read', 'file:///Users/me/secret.txt').includes('file:'), 'a refusal names the scheme, not the address');
const ERROR_PAGE = 'chrome-error://chromewebdata/';
check(isRemoteTabUrl(ERROR_PAGE) && !isRemotePageUrl(ERROR_PAGE), 'an error page stays a tab');
check(['navigate', 'history', 'close', 'tabs', 'select'].every((op) => remoteRefusal(op, ERROR_PAGE) === null), 'an error page can be left and closed');
check(['read', 'links', 'eval', 'act', 'screenshot', 'wait', 'scroll', 'focus', 'dialog', 'console', 'network'].every((op) => remoteRefusal(op, ERROR_PAGE)), 'and nothing else');
const TOKEN = 'abcdefghijklmnopqrstuv12';
check(isRemoteFile(TOKEN, 'index.html') && isRemoteFile(TOKEN, 'report/in%20dex.html') && isRemoteFile(TOKEN, 'a/ü.html') && isRemoteFile(TOKEN, '..hidden/x%2e') && isRemoteFile(TOKEN, '...'), 'a token and a relative path');
for (const [token, rel] of [['Abcdefghijklmnopqrstuv12', 'a'], ['1bcdefghijklmnopqrstuv12', 'a'], [TOKEN + 'x', 'a'], [undefined, 'a'], [TOKEN, ''], [TOKEN, '/etc/passwd'], [TOKEN, '../a'], [TOKEN, 'a/../b'], [TOKEN, './a'], [TOKEN, 'a//b'], [TOKEN, 'a\\b'], [TOKEN, 'a\nb'], [TOKEN, 'a/%2e%2E/x'], [TOKEN, '%2e/x'], [TOKEN, '.%2E'], [TOKEN, 'a%5cx'], [TOKEN, 'a%2fx'], [TOKEN, 'a%2Fx'], [TOKEN, 'x%00'], [TOKEN, 'x?y'], [TOKEN, 'x#y'], [TOKEN, 'a/'], [TOKEN, 7]]) {
  check(!isRemoteFile(token, rel), `no shown file: ${JSON.stringify([token, rel])}`);
}
console.log('ok: the pages and files of a remote attach');

// Its input queue: one op at a time, in order, the modifiers up between two; an op whose
// deadline passes while it waits never runs
{
  const trace = [];
  const keyboard = { up: async (key) => { trace.push(`up ${key}`); if (key === 'Alt') throw new Error('gone'); } };
  const lane = new InputLane({ release: () => releaseModifiers(keyboard) });
  const soon = () => ({ deadlineAt: Date.now() + 2000, deadlineMs: 2000 });
  const op = (name, ms, out) => async () => { trace.push(`start ${name}`); await sleep(ms); trace.push(`end ${name}`); if (out instanceof Error) throw out; return out; };
  const UP = ['up Shift', 'up Control', 'up Alt', 'up Meta'];
  const first = lane.run(op('a', 30, 1), soon());
  const second = lane.run(op('b', 0, new Error('Unknown key: "Nope"')), soon());
  const third = lane.run(op('c', 0, 3), soon());
  let thrown = null;
  second.catch((err) => { thrown = err; });
  check(await first === 1 && await third === 3 && thrown && /Nope/.test(thrown.message), 'every op answers its own outcome');
  await lane.tail;
  check(JSON.stringify(trace) === JSON.stringify(['start a', 'end a', ...UP, 'start b', 'end b', ...UP, 'start c', 'end c', ...UP]), `one at a time, modifiers up after each, also after one that threw (${trace})`);

  // a deadline answers but does not stop the op: the next one waits for its real end
  trace.length = 0;
  const codes = {};
  const slow = lane.run(op('slow', 120), { deadlineAt: Date.now() + 40, deadlineMs: 40 }).catch((err) => { codes.slow = err; });
  const expired = lane.run(op('expired', 0), { deadlineAt: Date.now() + 60, deadlineMs: 60 }).catch((err) => { codes.expired = err; });
  const after = lane.run(op('after', 0, 'ran'), soon());
  await Promise.all([slow, expired]);
  check(codes.slow.code === 'browser_timeout' && codes.slow.message === 'act exceeded 40 ms', `the running one says it exceeded its deadline (${codes.slow.message})`);
  check(codes.expired.code === 'browser_timeout' && codes.expired.herdr && /did not run and nothing was done/.test(codes.expired.message), `the waiting one says nothing was done (${codes.expired.message})`);
  check(await after === 'ran', 'the next one still runs');
  await lane.tail;
  check(JSON.stringify(trace) === JSON.stringify(['start slow', 'end slow', ...UP, 'start after', 'end after', ...UP]), `an expired op sends nothing, not even a key up (${trace})`);

  // release holds the queue, not the answer (a page dialog blocks the key events until it is answered)
  let letGo;
  let released = 0;
  const blocked = new InputLane({ release: () => new Promise((resolve) => { released++; letGo = resolve; }) });
  check(await blocked.run(async () => 'answered', soon()) === 'answered' && released === 1, 'the answer does not wait for the release');
  let ran = false;
  const next = blocked.run(async (left) => { ran = left > 0 && left <= 2000; return 'next'; }, soon());
  let dialogError = null;
  const guarded = blocked.run(async () => { ran = 'guarded ran'; }, { ...soon(), guard: async () => { throw Object.assign(new Error('a dialog is open'), { code: 'dialog_open' }); } });
  await guarded.catch((err) => { dialogError = err; });
  await sleep(30);
  check(dialogError && dialogError.code === 'dialog_open' && ran === false, 'a guard that rejects ends the wait at once');
  letGo();
  check(await next === 'next' && ran === true, 'the queue goes on once the release is through, with what is left of the deadline');
  letGo();
  await blocked.tail;
  check(ran === true && blocked.size === 0, 'the guarded op never ran');

  // a turn also waits for `ready` (the overlay), inside the same deadline
  const unready = new InputLane({ release: async () => {} });
  let late = null;
  await unready.run(async () => { ran = 'unready ran'; }, { deadlineAt: Date.now() + 30, deadlineMs: 30, ready: sleep(60) }).catch((err) => { late = err; });
  await unready.tail;
  check(late && /did not run/.test(late.message) && ran === true, 'an op whose turn came but was not ready in time never ran');
  // the timer may be late (a busy process): the turn looks at the clock itself
  late = null;
  await unready.run(async () => { ran = 'past ran'; }, { deadlineAt: Date.now() - 1, deadlineMs: 30 }).catch((err) => { late = err; });
  await unready.tail;
  check(late && /did not run/.test(late.message) && ran === true, 'an op whose turn comes after its deadline never runs');
}
console.log('ok: remote input ops run one at a time');

// host.mjs itself, over its pipe, with a fake playwright-core: pages in memory, a control page,
// a keyboard that tracks its held modifiers. `eval` with a `fake:` expression reaches the fake.
const FAKE_PLAYWRIGHT = `
import { EventEmitter } from 'node:events';
export const fake = { trace: [], gate: null, attaches: 0, controlCalls: [], keyMs: 100, shot: { width: 3000, height: 2000 } };
const png = (w, h, size = 64) => { const b = Buffer.alloc(size, 7); b.writeUInt32BE(w, 16); b.writeUInt32BE(h, 20); return b.toString('base64'); };
fake.png = png;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
class Page extends EventEmitter {
  constructor(target, at) {
    super();
    this.target = target; this.at = at; this.frame = { url: () => this.at };
    const held = new Set(); // a page's own, as Playwright's
    this.keyboard = {
      press: async (chord) => {
        fake.trace.push('press ' + chord);
        const parts = chord.split('+');
        for (const key of parts.slice(0, -1)) held.add(key);
        await sleep(fake.keyMs);
        if (fake.gate) await fake.gate.promise;
        if (parts.at(-1) === 'Nope') throw new Error('Unknown key: "Nope"');
        fake.trace.push('got ' + [...held, parts.at(-1)].join('+'));
        for (const key of parts.slice(0, -1)) held.delete(key);
      },
      up: async (key) => { fake.trace.push('up ' + key); held.delete(key); },
    };
  }
  url() { return this.at; }
  go(url) { this.at = url; this.emit('framenavigated', this.frame); }
  isClosed() { return false; }
  mainFrame() { return this.frame; }
  context() { return ctx; }
  async title() { return 'title of ' + this.target; }
  async evaluate(fn, arg) {
    if (typeof fn === 'string' && fn.startsWith('fake:')) return (0, eval)('(fake) => (' + fn.slice(5) + ')')(fake);
    if (this.at.startsWith('herdr-companion://control/')) { globalThis.__herdrCompanion = api; return fn(arg); }
    if (typeof fn === 'function' && /visibilityState/.test(String(fn))) return /!==/.test(String(fn)) ? false : true;
    return undefined;
  }
  async evaluateHandle() { return { asElement: () => null, dispose: async () => {} }; }
  async waitForLoadState() {}
  async waitForURL(match) { for (let i = 0; i < 100 && !match(new URL(this.at)); i++) await sleep(10); }
  async route() { fake.trace.push('route ' + this.target); }
  async routeWebSocket() {}
  async exposeBinding(_name, fn) { fake.binding = fn; }
}
const ctx = new EventEmitter();
ctx.pages = () => fake.pages;
ctx.newCDPSession = async (page) => ({
  on() {}, async detach() {},
  async send(method, params) {
    if (method === 'Target.getTargetInfo') return { targetInfo: { targetId: page.target } };
    if (method === 'Page.captureScreenshot') return { data: png(...(params.clip && params.clip.scale < 1 ? [1568, 1045] : [fake.shot.width, fake.shot.height, fake.shot.size])) };
    if (method === 'Page.getLayoutMetrics') return { cssVisualViewport: { pageX: 0, pageY: 0, clientWidth: 1500, clientHeight: 1000 }, cssContentSize: { width: 1500, height: 4000 } };
    if (method === 'Page.getFrameTree') return { frameTree: { frame: { id: 'f' } } };
    if (method === 'Page.createIsolatedWorld') return { executionContextId: 1 };
    if (method === 'Runtime.evaluate') { fake.trace.push('overlay ' + page.target); if (fake.onOverlay) { const moved = fake.onOverlay; fake.onOverlay = null; moved(); } return { result: { value: null } }; }
    return {};
  },
});
const dialogs = { seq: 0, list: [] };
const push = () => { dialogs.seq++; fake.binding({}, { seq: dialogs.seq, dialogs: dialogs.list }); };
const api = {
  nonce: () => 'n-1',
  views: () => fake.pages.map((p) => ({ view_id: 'v' + p.target, target_id: p.target })),
  dialogs: () => ({ seq: dialogs.seq, dialogs: dialogs.list }),
  dialog: (arg) => { fake.trace.push('dialog ' + arg.accept); dialogs.list = []; push(); fake.free(); return {}; },
  paint: () => ({ held: true }),
  close: (view) => { fake.controlCalls.push(['close', view]); return {}; },
  showRemote: (arg) => { fake.controlCalls.push(['showRemote', arg]); const page = new Page('T-file', 'about:blank#herdr-view=v9'); fake.pages.push(page); ctx.emit('page', page); setTimeout(() => page.go('herdr-file://' + arg.token + '/' + arg.rel), 60); return { view_id: 'v9' }; },
};
fake.openDialog = () => { dialogs.list = [{ id: 'd1', view_id: 'vT-web', target_id: 'T-web', type: 'alert', message: 'hi' }]; push(); };
fake.hold = () => { let open; const promise = new Promise((r) => { open = r; }); fake.gate = { promise, open }; };
fake.free = () => { const gate = fake.gate; fake.gate = null; if (gate) gate.open(); };
fake.page = (target) => fake.pages.find((p) => p.target === target);
fake.open = (target, url) => { const page = new Page(target, url); fake.pages.push(page); ctx.emit('page', page); };
fake.start = () => {
  fake.pages = [new Page('T-control', 'herdr-companion://control/'), new Page('T-strip', 'herdr-companion://ui/strip.html'), new Page('T-web', 'https://example.test/'), new Page('T-other', 'http://localhost:3000/'), new Page('T-blank', 'about:blank'), new Page('T-err', 'chrome-error://chromewebdata/'), new Page('T-tools', 'devtools://devtools/bundled/devtools_app.html'), new Page('T-path', 'file:///Users/me/secret.txt'), new Page('T-ctl', 'https://ctl.test/')];
  fake.trace.length = 0; fake.controlCalls.length = 0;
};
export const chromium = { async connectOverCDP() {
  fake.attaches++; fake.start(); ctx.removeAllListeners();
  const browser = new EventEmitter();
  let up = true;
  browser.contexts = () => [ctx];
  browser.isConnected = () => up;
  browser.version = () => '1';
  browser.close = async () => { if (up) { up = false; browser.emit('disconnected'); } };
  return browser;
} };
`;
{
  const assets = fileURLToPath(new URL('../src/integration/assets/browser/', import.meta.url));
  const tmp = mkdtempSync(join(os.tmpdir(), 'herdr-remote-host-'));
  process.on('exit', () => { try { rmSync(tmp, { recursive: true, force: true }); } catch {} });
  for (const f of ['host.mjs', 'activity.mjs', 'extract.mjs', 'package.json']) cpSync(join(assets, f), join(tmp, f));
  const pw = join(tmp, 'node_modules/playwright-core');
  mkdirSync(pw, { recursive: true });
  writeFileSync(join(pw, 'package.json'), JSON.stringify({ name: 'playwright-core', version: '0.0.0', type: 'module', main: 'index.mjs' }));
  writeFileSync(join(pw, 'index.mjs'), FAKE_PLAYWRIGHT);
  const shots = join(tmp, 'shots');

  function startHost() {
    const child = spawn(process.execPath, [join(tmp, 'host.mjs')], { cwd: tmp, stdio: ['pipe', 'pipe', 'inherit'], env: { ...process.env, NODE_NO_WARNINGS: '1' } });
    const waiting = new Map();
    const events = [];
    let nextId = 1;
    readline.createInterface({ input: child.stdout }).on('line', (line) => {
      const msg = JSON.parse(line);
      if (msg.event) { events.push(msg); return; }
      const done = waiting.get(msg.id);
      waiting.delete(msg.id);
      if (done) done(msg);
    });
    const call = (op, fields = {}) => new Promise((resolve, reject) => {
      const id = nextId++;
      const timer = setTimeout(() => reject(new Error(`${op} never answered`)), 20000);
      waiting.set(id, (msg) => { clearTimeout(timer); resolve(msg); });
      child.stdin.write(JSON.stringify({ id, op, profile: 'companion', args: {}, deadline_ms: 5000, ...fields }) + '\n');
    });
    const ended = new Promise((resolve) => child.on('exit', resolve));
    return {
      call, events,
      attach: (args = {}) => call('attach', { args: { kind: 'companion', port: 1, nonce: 'n-1', ...args } }),
      fake: async (expr) => { const r = await call('eval', { target: 'T-ctl', args: { expr: `fake:${expr}` } }); check(r.ok, `fake eval ${JSON.stringify(r)}`); return r.result.value; },
      trace: async function () { const out = await this.fake('JSON.stringify(fake.trace.splice(0))'); return JSON.parse(out); },
      press: (key, fields = {}) => call('act', { target: 'T-web', args: { kind: 'press', key }, ...fields }),
      stop: () => { child.stdin.end(); return ended; },
    };
  }
  const keys = (trace) => trace.filter((line) => /^(press|got|up)/.test(line));
  const UP = ['up Shift', 'up Control', 'up Alt', 'up Meta'];
  const targets = (reply) => reply.result.tabs.map((t) => t.target).sort().join();

  // a local attach: as it always was
  {
    const host = startHost();
    const attached = await host.attach();
    check(attached.ok && targets(attached) === 'T-blank,T-ctl,T-err,T-other,T-path,T-tools,T-web', `locally every page but the companion's own is a tab (${targets(attached)})`);
    const [a, b] = await Promise.all([host.press('Shift+a'), host.press('b')]);
    check(a.ok && b.ok, 'both acts answer');
    const local = keys(await host.trace());
    check(JSON.stringify(local.slice(0, 2)) === JSON.stringify(['press Shift+a', 'press b']) && !local.some((l) => l.startsWith('up')), `locally input ops overlap and no key is released (${local})`);
    const unknown = await host.call('show_remote', { args: { token: TOKEN, rel: 'a.html' } });
    check(JSON.stringify(unknown.error) === JSON.stringify({ code: 'unknown_op', message: 'unknown op show_remote; run herdr browser setup' }), `locally show_remote is an unknown op (${JSON.stringify(unknown.error)})`);
    const shot = await host.call('screenshot', { target: 'T-web', args: { path: join(shots, 'a.png'), inline_path: join(shots, 'b.png'), format: 'png' } });
    check(shot.ok && JSON.stringify(Object.keys(shot.result)) === JSON.stringify(['path', 'inline_path', 'width', 'height', 'bytes']) && shot.result.path === join(shots, 'a.png') && shot.result.inline_path === join(shots, 'b.png'), `locally a screenshot answers its paths (${JSON.stringify(shot)})`);
    check(readdirSync(shots).sort().join() === 'a.png,b.png', 'and writes them');
    const read = await host.call('console', { target: 'T-tools' });
    check(read.ok && read.page.url.startsWith('devtools:'), 'locally no page is refused');
    await host.fake("fake.page('T-web').go('file:///Users/me/x')");
    check(host.events.some((e) => e.kind === 'navigated' && e.url === 'file:///Users/me/x'), 'locally every navigation is reported');
    check((await host.attach({ remote: false })).ok && await host.fake('fake.attaches') === 1, 'attaching again keeps the connection');
    rmSync(shots, { recursive: true, force: true });
    await host.stop();
  }
  console.log('ok: a local attach is unchanged');

  const host = startHost();
  const attached = await host.attach({ remote: true });
  check(attached.ok && targets(attached) === 'T-blank,T-ctl,T-err,T-other,T-web', `a remote attach has web, blank and error pages only (${targets(attached)})`);
  check((await host.call('read', { target: 'T-tools' })).error.code === 'tab_closed', 'DevTools is no tab');
  check((await host.call('read', { target: 'T-control' })).error.code === 'tab_closed', 'nor the control page');
  await host.fake("fake.page('T-tools').go('devtools://devtools/other.html')");
  check(targets(await host.call('tabs')) === 'T-blank,T-ctl,T-err,T-other,T-web', 'a DevTools page that moves stays none');
  await host.fake("fake.page('T-path').go('https://came.test/')");
  await sleep(50);
  check(targets(await host.call('tabs')) === 'T-blank,T-ctl,T-err,T-other,T-path,T-web' && host.events.some((e) => e.kind === 'opened' && e.target === 'T-path'), 'a page outside that came to a web page is a tab then');
  await host.fake("fake.open('T-popup', '')");
  await host.fake("fake.page('T-popup').go('https://popup.test/')");
  await sleep(50);
  check((await host.call('console', { target: 'T-popup' })).ok, 'so is a popup that had no address yet');
  console.log('ok: a remote attach tracks its allowlist only');

  // the check before every op
  const activity = { frame: true, animate: false, color: '#aa6eff', linger_ms: 1000 };
  await host.trace();
  host.events.length = 0;
  await host.fake("fake.page('T-other').go('file:///Users/me/secret.txt')");
  for (const [op, args] of [['read', {}], ['eval', { expr: 'document.title' }], ['screenshot', {}], ['act', { kind: 'press', key: 'a' }], ['navigate', { url: 'https://x.test/' }], ['console', {}], ['close', {}], ['focus', {}]]) {
    const r = await host.call(op, { target: 'T-other', args, activity });
    check(!r.ok && r.error.code === 'host_refused' && !JSON.stringify(r).includes('secret') && r.error.message.includes(op), `${op} on a page that moved is refused (${JSON.stringify(r)})`);
  }
  check(!(await host.trace()).some((l) => /T-other|press|up /.test(l)), 'and nothing reached the page, the overlay neither');
  check(!targets(await host.call('tabs')).includes('T-other') && !JSON.stringify(host.events).includes('secret'), 'its address is not told');
  await host.fake("fake.page('T-other').go('http://localhost:3000/back')");
  check((await host.call('console', { target: 'T-other' })).ok, 'back on a web page it is driven again');
  await host.fake("(fake.onOverlay = () => fake.page('T-other').go('devtools://devtools/'), 1)");
  const during = await host.call('console', { target: 'T-other', activity });
  check(during.error && during.error.code === 'host_refused', `a page that moves while the overlay goes up is refused (${JSON.stringify(during)})`);
  await host.fake("fake.page('T-other').go('http://localhost:3000/again')");
  const midway = await host.call('eval', { target: 'T-other', args: { expr: "fake:fake.page('T-other').go('file:///Users/me/secret.txt')" } });
  check(midway.ok && midway.page.url === 'about:blank' && midway.page.title === '' && !JSON.stringify(midway).includes('secret'), `one that moves during an op is not told in its answer (${JSON.stringify(midway)})`);
  await host.fake("fake.page('T-other').go('http://localhost:3000/end')");
  const err = await host.call('read', { target: 'T-err' });
  check(err.error && err.error.code === 'host_refused' && /did not load/.test(err.error.message), `an error page is not read (${JSON.stringify(err.error)})`);
  check((await host.call('close', { target: 'T-err' })).ok && JSON.parse(await host.fake('JSON.stringify(fake.controlCalls)'))[0][0] === 'close', 'but closed');
  for (const op of ['show', 'watch']) {
    const r = await host.call(op, { target: 'T-web', args: { file: fileURLToPath(import.meta.url) } });
    check(r.error && r.error.code === 'host_refused', `${op} takes a path here: refused (${JSON.stringify(r.error)})`);
  }
  console.log('ok: a remote attach checks the page before every op');

  // DevTools of a page or popup is a page target with the inspector's powers (Electron opens
  // it whatever the companion asks): no op reaches it, whether it comes as DevTools or a
  // tracked page becomes it
  const PAGE_OPS = [['read', { format: 'html' }], ['links'], ['screenshot', { format: 'png' }], ['console'], ['network'], ['wait', { text: 'x' }], ['scroll', { to: 'top' }], ['eval', { expr: 'InspectorFrontendHost' }], ['act', { kind: 'click', selector: 'a' }], ['act', { kind: 'press', key: 'a' }], ['act', { kind: 'type', selector: 'a', text: 'x' }], ['act', { kind: 'fill', selector: 'a', text: 'x' }], ['navigate', { url: 'https://x.test/' }], ['history', { action: 'reload' }], ['dialog', { accept: true }], ['focus'], ['select'], ['close']];
  await host.trace();
  host.events.length = 0;
  await host.fake("fake.open('T-dt', 'devtools://devtools/bundled/devtools_app.html?dockSide=bottom')");
  await host.fake("fake.open('T-dt2', 'about:blank')");
  await sleep(50);
  check((await host.call('console', { target: 'T-dt2' })).ok, 'a blank popup is a tab');
  await host.fake("fake.page('T-dt2').go('devtools://devtools/bundled/devtools_app.html')");
  await host.fake("fake.page('T-dt2').emit('domcontentloaded')");
  for (const [target, code] of [['T-dt', 'tab_closed'], ['T-dt2', 'host_refused']]) {
    for (const [op, args = {}] of PAGE_OPS) {
      const r = await host.call(op, { target, args, activity });
      check(!r.ok && r.error.code === code, `${op} on a DevTools target (${target}): ${JSON.stringify(r)}`);
    }
    await host.fake(`fake.page('${target}').go('devtools://devtools/bundled/inspector.html')`);
  }
  check(!targets(await host.call('tabs')).includes('T-dt'), 'nor is it listed');
  check(!(await host.trace()).some((l) => /T-dt|press|up /.test(l)), 'nothing reached it, the overlay neither');
  check(!host.events.some((e) => String(e.target).startsWith('T-dt') && /devtools/.test(JSON.stringify(e))) && !host.events.some((e) => e.target === 'T-dt'), `nor is it told (${JSON.stringify(host.events.filter((e) => String(e.target).startsWith('T-dt')))})`);
  console.log('ok: no op reaches DevTools');

  // the input queue of a page
  await host.trace();
  const [p1, p2, p3, p4] = await Promise.all([host.press('Shift+a', { activity }), host.press('Shift+Nope'), host.press('b', { activity }), host.call('act', { target: 'T-other', args: { kind: 'press', key: 'o' } })]);
  check(p1.ok && !p2.ok && /Nope/.test(p2.error.message) && p3.ok && p4.ok, 'every act answers its own outcome');
  await sleep(100);
  const queue = keys(await host.trace());
  const web = queue.filter((l) => !/ o$/.test(l));
  check(web.filter((l) => !l.startsWith('up')).join() === 'press Shift+a,got Shift+a,press Shift+Nope,press b,got b', `one at a time, in order, no modifier left (${queue})`);
  check(queue.indexOf('press o') < queue.indexOf('got Shift+a'), `another page's input does not wait (${queue})`);
  check(queue.filter((l) => l.startsWith('up')).length === 16 && queue.slice(queue.indexOf('press Shift+Nope') + 1, queue.indexOf('press b')).join().includes(UP.join()), `the four modifiers go up after each (${queue})`);

  await host.fake('fake.hold()');
  const running = host.press('a', { deadline_ms: 300 });
  await sleep(50);
  const t0 = Date.now();
  const expired = host.press('b', { deadline_ms: 400 });
  const moved = host.press('c');
  const readReply = await host.call('console', { target: 'T-web' });
  check(readReply.ok, 'other ops are not queued');
  const r1 = await running;
  check(r1.error.code === 'browser_timeout' && r1.error.message === 'act exceeded 300 ms', `the running act (${JSON.stringify(r1.error)})`);
  const r2 = await expired;
  check(r2.error.code === 'browser_timeout' && /did not run and nothing was done/.test(r2.error.message) && Date.now() - t0 < 1500, `the waiting one (${JSON.stringify(r2.error)})`);
  await host.fake("fake.page('T-web').go('devtools://devtools/')");
  await host.fake('fake.free()');
  const r3 = await moved;
  check(r3.error && r3.error.code === 'host_refused', `a page that moved while an act waited is refused at its turn (${JSON.stringify(r3)})`);
  await host.fake("fake.page('T-web').go('https://example.test/')");
  await sleep(50);
  const after = keys(await host.trace());
  check(JSON.stringify(after) === JSON.stringify(['press a', 'got a', ...UP]), `neither sent a key, not even a key up (${after})`);

  // a dialog is answered while the act that opened it still holds the queue
  await host.fake('fake.hold()');
  const opener = host.press('Enter', { deadline_ms: 4000 });
  await sleep(80);
  const behind = host.press('d', { deadline_ms: 4000 });
  await sleep(50);
  await host.fake('fake.openDialog()');
  const t1 = Date.now();
  const refused = await host.press('e', { deadline_ms: 4000 });
  check(refused.error && refused.error.code === 'dialog_open' && Date.now() - t1 < 2000, `an act that comes while a dialog is up hears of it at once (${JSON.stringify(refused)})`);
  const accepted = await host.call('dialog', { target: 'T-web', args: { accept: true } });
  check(accepted.ok && accepted.result.message === 'hi', `a dialog is never queued (${JSON.stringify(accepted)})`);
  check((await opener).ok && (await behind).ok, 'then the queue goes on');
  await sleep(50);
  check(keys(await host.trace()).filter((l) => l.startsWith('press')).join() === 'press Enter,press d', 'the refused act sent no key');
  console.log('ok: remote acts queue per page');

  // pictures come back in the answer; no path is written, even when one is sent
  const paths = { path: join(shots, 'a.png'), inline_path: join(shots, 'b.png'), format: 'png' };
  const shot = await host.call('screenshot', { target: 'T-web', args: paths });
  check(shot.ok && JSON.stringify(Object.keys(shot.result)) === JSON.stringify(['full', 'inline', 'width', 'height', 'bytes']), `a remote screenshot answers its bytes (${JSON.stringify(shot).slice(0, 300)})`);
  const full = Buffer.from(shot.result.full, 'base64');
  const inline = Buffer.from(shot.result.inline, 'base64');
  check(full.length === shot.result.bytes && full.readUInt32BE(16) === 3000 && shot.result.width === 3000 && inline.readUInt32BE(16) === 1568, 'the picture and its scaled copy');
  check(!existsSync(shots), 'no file is written here');
  const bare = await host.call('screenshot', { target: 'T-web', args: { format: 'png' } });
  check(bare.ok && bare.result.full === shot.result.full && bare.result.inline === shot.result.inline, 'the same without paths (the gate drops them)');
  await host.fake('(fake.shot = { width: 800, height: 600 }, 1)');
  const small = await host.call('screenshot', { target: 'T-web', args: { format: 'png' } });
  check(small.ok && small.result.inline === null && small.result.width === 800, 'a small one has no scaled copy');
  await host.fake('(fake.shot = { width: 800, height: 600, size: 32 * 1024 * 1024 + 1 }, 1)');
  const huge = await host.call('screenshot', { target: 'T-web', args: { format: 'png' } });
  check(huge.error && huge.error.code === 'too_large', `one over the link's limit is an error (${JSON.stringify(huge).slice(0, 200)})`);
  console.log('ok: remote screenshots answer bytes');

  // show_remote: a token and a relative path, in the companion's sandboxed view
  for (const args of [{ token: TOKEN, rel: '../x.html' }, { token: TOKEN, rel: '/etc/passwd' }, { token: 'x', rel: 'a.html' }, { file: '/etc/passwd' }]) {
    const r = await host.call('show_remote', { args });
    check(r.error && r.error.code === 'invalid_request', `refused: ${JSON.stringify(args)}`);
  }
  const shown = await host.call('show_remote', { args: { token: TOKEN, rel: 'out/report.html', tab_id: 'w1:t1', opened_by: 'w1:p2', title: 'Report', focus: true, file: '/etc/passwd', root: '/', allow_network: true, page_api: true, opener: 'w1:p2', watch: true } });
  check(shown.ok && JSON.stringify(shown.result) === JSON.stringify({ target: 'T-file', view_id: 'v9', watching: false }), `the view (${JSON.stringify(shown)})`);
  check(shown.page.url === `herdr-file://${TOKEN}/out/report.html`, `answered once the file's page is there (${shown.page.url})`);
  const sent = JSON.parse(await host.fake('JSON.stringify(fake.controlCalls)')).filter((c) => c[0] === 'showRemote');
  check(JSON.stringify(sent) === JSON.stringify([['showRemote', { tab_id: 'w1:t1', opened_by: 'w1:p2', token: TOKEN, rel: 'out/report.html', focus: true, title: 'Report' }]]), `the companion gets no path, network, page_api or opener (${JSON.stringify(sent)})`);
  check((await host.trace()).includes('route T-file'), 'its network is blocked');
  check((await host.call('console', { target: 'T-file' })).ok, 'and it is driven like any tab');
  console.log('ok: show_remote');

  // the flag belongs to the connection
  check((await host.attach({ remote: true })).ok && await host.fake('fake.attaches') === 1, 'the same attach again keeps the connection');
  const local = await host.attach();
  check(local.ok && await host.fake('fake.attaches') === 2 && targets(local).includes('T-tools'), 'a local attach to the same port connects anew');
  const again = await host.attach({ remote: true });
  check(again.ok && await host.fake('fake.attaches') === 3 && targets(again) === 'T-blank,T-ctl,T-err,T-other,T-web', `and a remote one filters again (${targets(again)})`);
  check(targets(await host.attach({ remote: 'yes' })).includes('T-tools'), 'only true is remote');
  await host.stop();
  console.log('ok: a remote attach never shares a connection with a local one');
}
