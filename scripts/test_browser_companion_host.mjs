// Companion browser host (fork): the sidecar's control-page driver and the
// HTML view watches, against a fake control page (no Electron, no
// Chromium): the nonce check, view calls and their error codes, the
// target -> view map, focus and raise against old and current companions,
// furniture URLs, debounced reloads and the watch cap.
// Run: node scripts/test_browser_companion_host.mjs   (also run by
// integration::browser_assets::tests when node is on PATH)
import { ControlPage, ViewWatches, isFurnitureUrl, CONTROL_URL_PREFIX } from '../src/integration/assets/browser/activity.mjs';

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
const watches = new ViewWatches({ watchFn, debounceMs: 30, max: 2 });
let reloads = 0;
check(watches.arm('T1', '/tmp/views/report.html', () => { reloads++; }), 'armed');
check(watchers.map((w) => w.path).join(',') === '/tmp/views/report.html,/tmp/views', 'the file and its directory are watched');
for (let i = 0; i < 5; i++) watchers[i % 2].cb('change', 'report.html');
await sleep(80);
check(reloads === 1, `a burst of changes is one reload (${reloads})`);
watches.arm('T2', '/tmp/views/b.html', () => {});
failed = null;
try { watches.arm('T3', '/tmp/views/c.html', () => {}); } catch (err) { failed = err; }
check(failed && failed.code === 'too_many_watches', 'the cap holds');
check(watches.arm('T1', '/tmp/views/report.html', () => { reloads++; }), 're-arming a watched view is not over the cap');
check(watchers.slice(0, 2).every((w) => w.closed), 're-arming closes the old watchers');
check(watches.disarm('T1') && !watches.has('T1') && !watches.disarm('T1'), 'disarm once');
console.log('ok: html view watches debounce, cap and disarm');
