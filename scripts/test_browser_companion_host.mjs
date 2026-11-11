// Companion browser host (fork): the sidecar's control-page driver and the
// HTML view watches, against a fake control page (no Electron, no
// Chromium): the nonce check, view calls and their error codes, the
// target -> view map, focus and raise against old and current companions,
// furniture URLs, the view watches (only the page and the files it
// references reload it, a half-written, growing or mid-rename page waits for
// its complete file, one reload at a time, the watch cap), the companion's page
// dialogs (a Playwright-like Dialog over the control page, push reconcile).
// Run: node scripts/test_browser_companion_host.mjs   (also run by
// integration::browser_assets::tests when node is on PATH)
import { ControlPage, ViewWatches, refsInside, reloadKeepingState, captureViewState, restoreViewState, isFurnitureUrl, CONTROL_URL_PREFIX, CompanionDialog, reconcileDialogs, pruneAnswered } from '../src/integration/assets/browser/activity.mjs';

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
