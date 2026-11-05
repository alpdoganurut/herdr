// End-to-end check of the companion against the mock herdr (P3.2-P3.5 acceptance).
// Starts ONE headless Electron at a time (never shown, never focused, no dock icon) and
// quits it through its own control page. Run: npm run e2e
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { fileURLToPath } from 'node:url';
import { createMockHerdr } from './mock-herdr.mjs';

const require = createRequire(import.meta.url);
const electronBin = require('electron');
const { Connection } = require('../src/herdr-api.js');
const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'hc-e2e-'));
const sock = path.join(tmp, 'h.sock');
const profile = path.join(tmp, 'profile');
const results = [];
let failed = 0;
const check = (name, ok, detail) => {
  results.push({ name, ok: !!ok, detail });
  if (!ok) failed++;
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail !== undefined ? ' — ' + JSON.stringify(detail).slice(0, 300) : ''}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, ms = 10000, step = 100) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try {
      last = await fn();
      if (last) return last;
    } catch (e) {
      last = e;
    }
    await sleep(step);
  }
  return null;
}

const mock = createMockHerdr({ socketPath: sock, verifyCdp: true, heartbeatMs: 1000, ttlMs: 8000 });
await mock.listen();

// One request per connection (the mock, like herdr, closes after answering).
async function api(method, params) {
  const client = new Connection(sock, { prefix: 't' });
  client.on('error', () => {});
  await client.connect();
  try {
    return await client.request(method, params);
  } finally {
    client.close();
  }
}
const ctl = async (expr) => (await api('mock.control', { expression: expr })).value;

let child = null;
const logFile = path.join(tmp, 'companion.log');
function startCompanion() {
  const out = fs.openSync(logFile, 'a');
  // fake media devices: getUserMedia reaches the permission prompt in a headless run
  child = spawn(electronBin, [appDir, '--socket', sock, '--profile-dir', profile, '--headless', '--use-fake-device-for-media-stream'], {
    env: { ...process.env, HERDR_COMPANION_HEADLESS: '1', HERDR_COMPANION_TEST_HOOKS: '1', HERDR_SOCKET_PATH: '' },
    stdio: ['ignore', out, out],
  });
  child.exited = new Promise((r) => child.on('exit', (code) => r(code)));
}
// Quits through the control page: the mock's CDP link, else a direct CDP connection
// to the port the companion wrote (never a signal).
async function quitDirect() {
  const { chromium } = require('playwright-core');
  const port = Number(fs.readFileSync(path.join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]);
  const b = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 10000 });
  try {
    const p = b.contexts().flatMap((c) => c.pages()).find((x) => x.url().startsWith('herdr-companion://control/'));
    if (p) await p.evaluate(() => globalThis.__herdrCompanion.quit());
  } finally {
    await b.close().catch(() => {});
  }
}
async function quitCompanion() {
  if (!child) return true;
  try {
    await ctl('__herdrCompanion.quit()');
  } catch {
    await quitDirect().catch((e) => console.log('direct quit failed:', e.message));
  }
  const code = await Promise.race([child.exited, sleep(30000).then(() => 'timeout')]);
  if (code === 'timeout') {
    console.log(`companion did not exit; abandoned pid ${child.pid}`);
    return false;
  }
  child = null;
  return true;
}

async function pageForTarget(targetId) {
  const browser = mock.state.browser;
  for (const ctx of browser.contexts()) {
    for (const p of ctx.pages()) {
      const s = await ctx.newCDPSession(p);
      const ti = await s.send('Target.getTargetInfo');
      await s.detach();
      if (ti.targetInfo.targetId === targetId) return p;
    }
  }
  return null;
}
const stripPage = () => mock.state.browser.contexts()[0].pages().find((p) => p.url().startsWith('herdr-companion://ui/strip.html'));

// a local http server to observe html-view network blocking
let hits = 0;
const webAgents = []; // user-agent headers seen
// pages declaring window drag regions (Slack's desktop CSS); /dragframe is embedded cross-site
const DRAG_CSS = '<style>#d{app-region:drag !important;-webkit-app-region:drag !important;height:40px}#d::before{content:"";app-region:drag}</style>';
const web = http.createServer((req, res) => {
  hits++;
  webAgents.push(req.headers['user-agent'] || '');
  const u = req.url.split('?')[0];
  res.setHeader('content-type', 'text/html');
  if (u === '/drag1' || u === '/drag2') return res.end(`<title>${u}</title>${DRAG_CSS}<div id=d>d</div><iframe src="http://localhost:${web.address().port}/dragframe"></iframe>`);
  if (u === '/dragframe') return res.end(`<title>frame</title>${DRAG_CSS}<div id=d>f</div>`);
  res.end('<title>web</title>web');
});
await new Promise((r) => web.listen(0, '127.0.0.1', r));
const webPort = web.address().port;

// pages for the frame dialogs (127.0.0.1 and localhost are two sites: their frames run in
// processes of their own)
const dlgWeb = http.createServer((req, res) => {
  const P = dlgWeb.address().port;
  const pages = {
    '/top': `<title>top</title><iframe id=x src="http://localhost:${P}/inner"></iframe>`,
    '/nest': `<title>nest</title><iframe id=a src="http://localhost:${P}/mid"></iframe>`,
    '/mid': `<title>mid</title><iframe id=b src="http://localhost:${P}/inner"></iframe>`,
    '/inner': '<title>inner</title><p>inner</p>',
    '/opaque': '<title>opaque</title><iframe sandbox="allow-scripts allow-modals" src="/inner"></iframe>',
    '/srcdoc': '<title>srcdoc</title><iframe sandbox="allow-scripts allow-modals" srcdoc="<p>sandboxed</p>"></iframe><iframe id=d src="data:text/html,<p>data</p>"></iframe>',
    '/leave':
      '<title>leave</title><script>' +
      "addEventListener('beforeunload', () => { localStorage.bu = String(prompt('leaving?', 'x')); });" +
      "addEventListener('pagehide', () => { localStorage.ph = String(confirm('really?')); alert('bye'); });" +
      '</script>',
    '/landed': '<title>landed</title><p>landed</p>',
  };
  res.setHeader('content-type', 'text/html');
  res.end(pages[req.url.split('?')[0]] || '<title>other</title>other');
});
await new Promise((r) => dlgWeb.listen(0, r));
const dlgPort = dlgWeb.address().port;

try {
  startCompanion();
  const lease = await waitFor(() => mock.state.lease, 40000);
  check('registers and passes the nonce check over CDP', lease, lease && { instance: lease.instance });
  const instance1 = lease && lease.instance;

  check('views() starts empty', (await ctl('__herdrCompanion.views()')).length === 0);
  const o1 = await ctl(`__herdrCompanion.open({tab_id:'w1:t1', url:'data:text/html,<title>one</title><button id=b onclick="window.c=(window.c||0)+1">b</button>', background:false})`);
  check('open answers view_id + target_id', o1 && o1.view_id && o1.target_id, o1);
  const p1 = o1 && (await waitFor(() => pageForTarget(o1.target_id), 5000));
  check('target_id is the Playwright page of the view', !!p1);
  const synced1 = await waitFor(() => {
    const t = mock.state.tabs.get('w1:t1');
    return t.browser && t.browser.entries.length === 1 && t.browser.entries[0].title === 'one' && t.browser.entries[0].target === o1.target_id ? t.browser : null;
  });
  check('host_sync carries the new page with target and title', synced1, synced1 && { key: synced1.key, active: synced1.active });

  const o2 = await ctl(`__herdrCompanion.open({tab_id:'w1:t2', url:'data:text/html,<title>two</title>two'})`);
  let views = await ctl('__herdrCompanion.views()');
  check('a page opened for another herdr tab stays hidden', views.find((v) => v.view_id === o2.view_id && !v.shown) && views.find((v) => v.view_id === o1.view_id && v.shown), views.map((v) => [v.tab_id, v.shown]));

  await api('mock.focus_tab', { tab_id: 'w1:t2' });
  views = await waitFor(async () => {
    const v = await ctl('__herdrCompanion.views()');
    return v.find((x) => x.view_id === o2.view_id && x.shown) ? v : null;
  }, 3000);
  check('tab.focused swaps the shown group', views);
  const shown = await waitFor(() => mock.state.reports.find((r) => r.action === 'shown' && r.tab_id === 'w1:t2'), 3000);
  check('a follow swap is reported as user_action shown', shown);

  // hidden view still works for agents (spike S3 in the app)
  await p1.click('#b');
  const png = await p1.screenshot();
  check('a hidden view clicks and screenshots', (await p1.evaluate(() => window.c)) === 1 && png.length > 1000, { bytes: png.length });

  // page dialogs: alert / confirm / prompt in the companion's own card (Electron has no prompt())
  {
    await api('mock.focus_tab', { tab_id: 'w1:t1' });
    await waitFor(async () => (await ctl('__herdrCompanion.views()')).find((v) => v.view_id === o1.view_id && v.shown), 3000);
    const dialogs = async () => (await ctl('__herdrCompanion.dialogs()')).dialogs;
    const cardPage = () => mock.state.browser.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith('herdr-companion://ui/dialog.html'));
    const card = () =>
      cardPage().evaluate(() => {
        const f = document.getElementById('field');
        const c = document.getElementById('card');
        return {
          hidden: c.hidden,
          type: c.dataset.type,
          origin: document.getElementById('origin').textContent,
          message: document.getElementById('message').textContent,
          field: !f.hidden,
          value: f.value,
          selected: [f.selectionStart, f.selectionEnd],
          cancel: !document.getElementById('cancel').hidden,
        };
      });
    const open = async (n = 1) => waitFor(async () => {
      const d = await dialogs();
      return d.length === n ? d : null;
    }, 3000);
    const settle = (p) => Promise.race([p.then((v) => ({ v }), (e) => ({ err: String(e.message || e).split('\n')[0] })), sleep(4000).then(() => ({ timeout: true }))]);
    check('the dialog card is a herdr-companion: furniture page', !!cardPage());

    // prompt: the card shows message + default (selected); typing + Enter answers the page
    let r = p1.evaluate(() => prompt('Copy this:', 'the text'));
    let ds = await open();
    check('prompt() opens a dialog herdr can see (type, message, default, target)', ds && ds[0].type === 'prompt' && ds[0].message === 'Copy this:' && ds[0].default_value === 'the text' && ds[0].target_id === o1.target_id, ds);
    let c = await waitFor(async () => ((x) => (!x.hidden && x.type === 'prompt' ? x : null))(await card()), 3000);
    check('the card shows the prompt with its default selected and OK/Cancel', c && c.field && c.value === 'the text' && c.selected[0] === 0 && c.selected[1] === 8 && c.cancel && /says$/.test(c.origin), c);
    await cardPage().fill('#field', 'typed here');
    await cardPage().press('#field', 'Enter');
    let got = await settle(r);
    check('typing and Enter returns the typed text', got.v === 'typed here', got);
    check('the card hides after the answer', await waitFor(async () => (await card()).hidden && (await dialogs()).length === 0, 3000));

    // answered through the control page (herdr's sidecar): default, text, dismiss
    r = p1.evaluate(() => prompt('q', 'dflt'));
    ds = await open();
    let ans = await ctl(`__herdrCompanion.dialog({view_id:${JSON.stringify(o1.view_id)}, accept:true})`);
    got = await settle(r);
    check('accept without text returns the default value', got.v === 'dflt' && ans && ans.ok && ans.type === 'prompt', { got, ans });
    r = p1.evaluate(() => prompt('q', 'dflt'));
    await open();
    await ctl(`__herdrCompanion.dialog({view_id:${JSON.stringify(o1.view_id)}, accept:true, text:'x'})`);
    got = await settle(r);
    check('accept with text returns it', got.v === 'x', got);
    r = p1.evaluate(() => prompt('q', 'dflt'));
    await open();
    await ctl(`__herdrCompanion.dialog({view_id:${JSON.stringify(o1.view_id)}, accept:false})`);
    got = await settle(r);
    check('dismiss returns null', got.v === null, got);
    r = p1.evaluate(() => prompt('q'));
    await open();
    await waitFor(async () => !(await card()).hidden, 3000);
    await cardPage().press('#field', 'Escape');
    got = await settle(r);
    check('Esc in the card returns null', got.v === null, got);
    const none = await ctl(`__herdrCompanion.dialog({view_id:${JSON.stringify(o1.view_id)}, accept:true})`);
    check('answering with no dialog open is no_dialog', none && none.error && none.error.code === 'no_dialog', none);

    // coercion as WebIDL DOMString
    r = p1.evaluate(() => prompt(undefined, null));
    ds = await open();
    check('prompt(undefined, null): message "" and default "null"', ds && ds[0].message === '' && ds[0].default_value === 'null', ds);
    await ctl(`__herdrCompanion.dialog({view_id:${JSON.stringify(o1.view_id)}, accept:true})`);
    got = await settle(r);
    check('… and returns "null" on OK', got.v === 'null', got);
    got = await settle(p1.evaluate(() => { try { prompt(Symbol('s')); return 'no throw'; } catch (e) { return e.name; } }));
    check('prompt(Symbol()) throws a TypeError and opens nothing', got.v === 'TypeError' && (await dialogs()).length === 0, got);

    // confirm and alert
    r = p1.evaluate(() => confirm('sure?'));
    await open();
    c = await waitFor(async () => ((x) => (!x.hidden && x.type === 'confirm' ? x : null))(await card()), 3000);
    check('confirm shows OK and Cancel, no field', c && !c.field && c.cancel && c.message === 'sure?', c);
    await cardPage().click('#ok');
    got = await settle(r);
    check('confirm OK returns true', got.v === true, got);
    r = p1.evaluate(() => confirm('sure?'));
    await open();
    await waitFor(async () => !(await card()).hidden, 3000);
    await cardPage().click('#cancel');
    got = await settle(r);
    check('confirm Cancel returns false', got.v === false, got);
    r = p1.evaluate(() => { const t = Date.now(); const v = alert('hello'); return { v: v === undefined ? 'undefined' : v, waited: Date.now() - t }; });
    await open();
    c = await waitFor(async () => ((x) => (!x.hidden && x.type === 'alert' ? x : null))(await card()), 3000);
    check('alert shows only OK', c && !c.field && !c.cancel && c.message === 'hello', c);
    await sleep(300);
    const blocked = await Promise.race([r.then(() => 'returned'), sleep(200).then(() => 'blocked')]);
    check('alert blocks the page until answered', blocked === 'blocked');
    await cardPage().click('#ok');
    got = await settle(r);
    check('alert returns undefined after OK', got.v && got.v.v === 'undefined' && got.v.waited >= 400, got);

    // the dialog belongs to its page's herdr tab: hidden with it, back when it shows again
    const po2 = await waitFor(() => pageForTarget(o2.target_id), 3000);
    r = po2.evaluate(() => prompt('from the other tab', 'keep'));
    ds = await open();
    check('a hidden page\'s prompt is open for herdr', ds && ds[0].view_id === o2.view_id, ds);
    await sleep(300);
    check('… but its card stays hidden while another tab shows', (await card()).hidden);
    await api('mock.focus_tab', { tab_id: 'w1:t2' });
    c = await waitFor(async () => ((x) => (!x.hidden && x.message === 'from the other tab' ? x : null))(await card()), 3000);
    check('switching to its herdr tab shows its card', c && c.value === 'keep', c);
    await cardPage().fill('#field', 'kept value');
    await api('mock.focus_tab', { tab_id: 'w1:t1' });
    check('switching away hides it again', await waitFor(async () => (await card()).hidden, 3000));
    await api('mock.focus_tab', { tab_id: 'w1:t2' });
    c = await waitFor(async () => ((x) => (!x.hidden && x.message === 'from the other tab' ? x : null))(await card()), 3000);
    check('… and back keeps what was typed', c && c.value === 'kept value', c);
    await cardPage().press('#field', 'Enter');
    got = await settle(r);
    check('the answer reaches the other tab\'s page', got.v === 'kept value', got);

    // a navigation or a close while a dialog is open: the page's call returns null, nothing hangs
    const ow = await ctl(`__herdrCompanion.open({tab_id:'w1:t2', url:'http://127.0.0.1:${webPort}/dlg', background:false})`);
    const pw = await waitFor(() => pageForTarget(ow.target_id), 5000);
    await pw.waitForLoadState('load').catch(() => {});
    r = pw.evaluate(() => prompt('reload me'));
    await open();
    await stripPage().evaluate(() => window.stripBridge.action('reload'));
    got = await settle(r);
    check('a reload while a prompt is open cancels it', !got.timeout && (await dialogs()).length === 0, got);
    const alive = await waitFor(async () => ((x) => (x.v === 2 ? x : null))(await settle(pw.waitForLoadState('load').then(() => pw.evaluate(() => 1 + 1)))), 5000);
    check('… and the same-origin reload completes', alive && alive.v === 2, alive);
    r = pw.evaluate(() => prompt('close me'));
    await open();
    await ctl(`__herdrCompanion.close(${JSON.stringify(ow.view_id)})`);
    got = await settle(r);
    check('closing the page while a prompt is open answers it (no hang)', !got.timeout && (await dialogs()).length === 0, got);
    await api('mock.focus_tab', { tab_id: 'w1:t2' });

    // a long message scrolls inside the card: the field and the buttons stay in the view
    {
      const ol = await ctl(`__herdrCompanion.open({tab_id:'w1:t2', url:'data:text/html,<title>long</title>long', background:false})`);
      const pl = await waitFor(() => pageForTarget(ol.target_id), 5000);
      r = pl.evaluate(() => prompt(Array.from({ length: 400 }, (_, i) => `line ${i}`).join('\n'), 'd'));
      r.catch(() => {});
      await open();
      await waitFor(async () => !(await card()).hidden, 3000);
      await sleep(300);
      const fit = await cardPage().evaluate(() => {
        const m = document.getElementById('message');
        const ok = document.getElementById('ok').getBoundingClientRect();
        return { scrolls: m.scrollHeight > m.clientHeight, max: m.style.maxHeight, okBottom: Math.round(ok.bottom), inner: innerHeight };
      });
      check('a long message scrolls in the card and OK stays in view', fit.scrolls && fit.okBottom <= fit.inner, fit);
      await ctl(`__herdrCompanion.close(${JSON.stringify(ol.view_id)})`);
      await settle(r);
    }

    // frames: an embedded page's dialog says so, a frame that goes away takes its dialog along
    const D = `http://127.0.0.1:${dlgPort}`;
    const openPage = async (url) => {
      const o = await ctl(`__herdrCompanion.open({tab_id:'w1:t2', url:${JSON.stringify(url)}, background:false})`);
      const pg = await waitFor(() => pageForTarget(o.target_id), 5000);
      await pg.waitForLoadState('load').catch(() => {});
      return { o, pg };
    };
    const frameOf = (pg, pred) => waitFor(async () => pg.frames().find((f) => f !== pg.mainFrame() && pred(f)) || null, 5000);
    const cardShows = (message) => waitFor(async () => ((x) => (!x.hidden && x.message === message ? x : null))(await card()), 3000);
    const answerShown = (viewId) => ctl(`__herdrCompanion.dialog({view_id:${JSON.stringify(viewId)}, accept:false})`);
    let fo = await openPage(`${D}/opaque`);
    let fr = await frameOf(fo.pg, () => true);
    r = fr.evaluate(() => prompt('from a sandboxed frame'));
    r.catch(() => {}); // the frame may go with its call
    await open();
    c = await cardShows('from a sandboxed frame');
    check('an opaque-origin frame\'s dialog reads as an embedded page, never as the page', c && c.origin === 'An embedded page says', c);
    await answerShown(fo.o.view_id);
    got = await settle(r);
    check('… and is answered', got.v === null, got);
    r = fo.pg.evaluate(() => prompt('from the page itself'));
    await open();
    c = await cardShows('from the page itself');
    check('the page\'s own dialog names its origin', c && c.origin === `${D} says`, c);
    await answerShown(fo.o.view_id);
    await settle(r);
    await ctl(`__herdrCompanion.close(${JSON.stringify(fo.o.view_id)})`);
    {
      // every frame's prompt is the companion's (Electron re-assigns its own in subframes)
      const sp = await openPage(`${D}/srcdoc`);
      await sleep(300);
      const kinds = [];
      for (const f of sp.pg.frames()) kinds.push(`${f.url().slice(0, 20)}: ${await f.evaluate(() => (/native code/.test(String(prompt)) ? 'native' : 'companion')).catch((e) => String(e.message).split('\n')[0])}`);
      check('every frame (cross-site, srcdoc, data:) has the companion\'s prompt', kinds.length === 3 && kinds.every((k) => k.endsWith(': companion')), kinds);
      await ctl(`__herdrCompanion.close(${JSON.stringify(sp.o.view_id)})`);
    }

    fo = await openPage(`${D}/top`);
    fr = await frameOf(fo.pg, (f) => f.url().includes('/inner'));
    r = fr.evaluate(() => prompt('from a removed frame'));
    r.catch(() => {}); // the frame may go with its call
    await open();
    c = await cardShows('from a removed frame');
    check('a cross-site frame\'s dialog names it as embedded', c && c.origin === `An embedded page at http://localhost:${dlgPort} says`, c);
    const removed = await settle(fo.pg.evaluate(() => (document.getElementById('x').remove(), 'removed')));
    check('the page removes the frame while its dialog is open (its own process is free)', removed.v === 'removed', removed);
    let t0 = Date.now();
    let left = await waitFor(async () => (await dialogs()).length === 0, 5000);
    check('the removed frame\'s dialog is cancelled (no stale card)', left && (await card()).hidden, { ms: Date.now() - t0 });
    r = fo.pg.evaluate(() => prompt('after the removal', 'p'));
    ds = await open();
    check('… and the page\'s own dialogs are not stuck behind it', ds && ds[0].message === 'after the removal', ds);
    await answerShown(fo.o.view_id);
    await settle(r);
    await ctl(`__herdrCompanion.close(${JSON.stringify(fo.o.view_id)})`);

    fo = await openPage(`${D}/nest`);
    fr = await frameOf(fo.pg, (f) => f.url().includes('/inner') && f.parentFrame() !== fo.pg.mainFrame());
    r = fr.evaluate(() => prompt('from a nested frame'));
    r.catch(() => {}); // the frame may go with its call
    await open();
    t0 = Date.now();
    const moved = await settle(fo.pg.evaluate((u) => ((document.getElementById('a').src = u), 'moved'), `http://localhost:${dlgPort}/landed`));
    left = await waitFor(async () => (await dialogs()).length === 0, 8000);
    check('a navigation of the frame around the one that asked cancels its dialog', moved.v === 'moved' && left, { moved, ms: Date.now() - t0 });
    got = await settle(r);
    check('… and nothing hangs', !got.timeout, got);
    await ctl(`__herdrCompanion.close(${JSON.stringify(fo.o.view_id)})`);

    // dialogs of a leaving page (beforeunload, pagehide) are answered at once and never shown
    fo = await openPage(`${D}/landed`);
    r = fo.pg.evaluate(() => {
      addEventListener('pagehide', () => { window.__got = prompt('a page event is not a leave', 's'); });
      dispatchEvent(new Event('pagehide'));
      return window.__got;
    });
    ds = await open();
    check('a pagehide the page dispatches itself does not silence its dialogs', ds && ds[0].message === 'a page event is not a leave', ds);
    await answerShown(fo.o.view_id);
    await settle(r);
    await ctl(`__herdrCompanion.close(${JSON.stringify(fo.o.view_id)})`);
    fo = await openPage(`${D}/leave`);
    let seen = 0;
    const watch = setInterval(async () => {
      try {
        if ((await dialogs()).length) seen++;
      } catch {}
    }, 50);
    await settle(fo.pg.evaluate((u) => ((location.href = u), 'go'), `${D}/landed`));
    const landed = await waitFor(async () => (fo.pg.url().endsWith('/landed') && (await fo.pg.title()) === 'landed') || null, 8000);
    await sleep(300);
    clearInterval(watch);
    const stored = landed && (await fo.pg.evaluate(() => ({ bu: localStorage.bu, ph: localStorage.ph })));
    check('a leaving page\'s prompt / confirm / alert return at once, never shown', landed && seen === 0 && stored && stored.bu === 'null' && stored.ph === 'false' && (await card()).hidden, { landed: !!landed, seen, stored });
    await fo.pg.evaluate(() => localStorage.clear());
    await ctl(`__herdrCompanion.close(${JSON.stringify(fo.o.view_id)})`);
  }

  // strip actions through the strip page (same IPC the buttons use)
  const strip = stripPage();
  check('strip page is a herdr-companion: furniture page', !!strip);
  const sendbarShown = await strip.evaluate(() => getComputedStyle(document.getElementById('sendbar')).display);
  check('the send form starts hidden', sendbarShown === 'none', sendbarShown);
  await strip.evaluate(() => window.stripBridge.action('new_tab'));
  await strip.evaluate((u) => window.stripBridge.action('navigate', u), `127.0.0.1:${webPort}/x`);
  const nav = await waitFor(() => mock.state.reports.find((r) => r.action === 'navigate' && r.url.includes(String(webPort))), 3000);
  check('URL box navigate is reported as a user action', nav);
  const navSynced = await waitFor(() => mock.state.tabs.get('w1:t2').browser.entries.find((e) => e.url.includes(`:${webPort}/x`) && e.title === 'web'), 5000);
  check('the navigated page syncs url and title', navSynced);
  await strip.evaluate(() => window.stripBridge.action('send_page', { note: 'look' }));
  const sent = await waitFor(() => mock.state.sent[0], 3000);
  check('send to agent calls browser.send_page with tab, url and note', sent && sent.tab_id === 'w1:t2' && sent.note === 'look' && sent.url.includes(String(webPort)), sent);
  // the compact bar: one row when wide, two when narrow; the URL compact until focused
  const barLayout = () =>
    strip.evaluate(() => {
      const top = (id) => Math.round(document.getElementById(id).getBoundingClientRect().top);
      return {
        narrow: document.getElementById('bar').classList.contains('narrow'),
        height: Math.round(document.getElementById('bar').getBoundingClientRect().height),
        tabs: top('tabs'),
        newtab: top('newtab'),
        url: top('urlform'),
        back: top('back'),
        width: document.documentElement.clientWidth,
      };
    });
  const wide = await barLayout();
  check('wide, the bar is one row (tabs, navigation and the URL side by side)', !wide.narrow && wide.height <= 32 && Math.abs(wide.url - wide.tabs) <= 6 && Math.abs(wide.back - wide.tabs) <= 6, wide);
  const viewport = await strip.evaluate(() => ({ width: innerWidth, height: innerHeight }));
  await strip.setViewportSize({ width: 420, height: viewport.height });
  const narrow = await waitFor(async () => {
    const l = await barLayout();
    return l.narrow ? l : null;
  }, 3000);
  check('narrow (420 px), it wraps into two rows: tabs and new tab, then navigation and the URL', narrow && narrow.height >= 48 && narrow.height <= 64 && Math.abs(narrow.newtab - narrow.tabs) <= 6 && narrow.url > narrow.tabs + 16 && Math.abs(narrow.back - narrow.url) <= 6, narrow);
  await strip.setViewportSize(viewport);
  const back = await waitFor(async () => {
    const l = await barLayout();
    return !l.narrow ? l : null;
  }, 3000);
  check('wide again, one row again', back && back.height <= 32, back);
  await strip.evaluate((u) => window.stripBridge.action('navigate', u), `127.0.0.1:${webPort}/docs/a%20page?q=1#part`);
  const compact = await waitFor(async () => {
    const v = await strip.evaluate(() => document.getElementById('url').value);
    return v.includes('a page') ? v : null;
  }, 5000);
  check('the URL shows host › last segment #fragment', compact === `127.0.0.1:${webPort} \u203a a page #part`, compact);
  const editing = await strip.evaluate(() => {
    const url = document.getElementById('url');
    url.focus();
    const out = { value: url.value, editing: document.getElementById('bar').classList.contains('editing') };
    url.blur();
    out.after = url.value;
    return out;
  });
  check('focused, the field shows the whole URL; blurred, the compact one again', editing.value === `http://127.0.0.1:${webPort}/docs/a%20page?q=1#part` && editing.editing && editing.after === compact, editing);
  // the count pill's menu: the shown herdr tab and Go to herdr tab, then the tabs with pages
  await strip.evaluate(() => window.stripBridge.action('sessions_menu'));
  const menu = await ctl('__herdrCompanion.sessions_menu()');
  check('the pill menu leads with this tab and Go to herdr tab', menu && /^This tab · /.test(menu[0]) && menu[1] === 'Go to herdr tab' && menu.includes('herdr tabs with pages'), menu);
  const chev = await strip.evaluate(() => !!document.querySelector('#count .chev'));
  check('the pill has a chevron and no session label next to it', chev && !(await strip.evaluate(() => document.getElementById('session'))), chev);
  // expand / restore / close: docked, and only from a herdr that advertises placement_actions
  const winButtons = () =>
    strip.evaluate(() => ({
      expand: !document.getElementById('expand').hidden,
      action: document.getElementById('expand').dataset.action || null,
      close: !document.getElementById('dockclose').hidden,
    }));
  check('the floating window shows neither expand nor close', !(await winButtons()).expand && !(await winButtons()).close, await winButtons());
  await ctl(`__herdrCompanion.strip_preview({dock: true, kind: 'side', features: ['placements']})`);
  await sleep(300);
  const older = await winButtons();
  check('docked under a herdr without placement_actions: no expand, no close', older && !older.expand && !older.close, older);
  await ctl(`__herdrCompanion.strip_preview({dock: true, kind: 'side', features: ['placements', 'placement_actions']})`);
  const side = await waitFor(async () => {
    const b = await winButtons();
    return b.expand ? b : null;
  }, 2000);
  check('docked under a herdr with placement_actions: expand (promote) and close', side && side.action === 'promote' && side.close, side);
  await ctl(`__herdrCompanion.strip_preview({dock: true, kind: 'full', features: ['placements', 'placement_actions']})`);
  const full = await waitFor(async () => {
    const b = await winButtons();
    return b.action === 'restore' ? b : null;
  }, 2000);
  check('in herdr\'s Browser view: restore and close', full && full.expand && full.close, full);
  await ctl(`__herdrCompanion.strip_preview({dock: true, kind: 'side', features: ['placements', 'placement_actions'], clientTakes: false})`);
  await sleep(300);
  const olderClient = await winButtons();
  check('docked over an older herdr client (it ignores the actions): no expand, no close', !olderClient.expand && !olderClient.close, olderClient);
  await ctl('__herdrCompanion.strip_preview(null)');
  check('back to the floating bar: neither', await waitFor(async () => !(await winButtons()).expand || null, 2000));
  // auto-hide and the pin, per surface (the floating window: pinned by default). The pointer
  // moves are CDP events into the strip page alone: they cover the strip's hover handler, not
  // the pointer crossing from the bar's view into a page view (a live check).
  const barInfo = () => ctl('__herdrCompanion.bar_debug()');
  const hiddenNow = () => strip.evaluate(() => document.documentElement.classList.contains('bar-hidden'));
  await strip.mouse.move(300, 500); // the pointer below the bar
  // a status message holds the hidden bar open (the send above flashed one for 6 s)
  check('a status message clears in time', await waitFor(() => strip.evaluate(() => document.getElementById('status').hidden || null), 9000, 100));
  await sleep(300);
  let bd = await barInfo();
  check('the floating bar is pinned by default: the page starts below it', bd && bd.layout.pinned && bd.layout.top === bd.report.win.height && bd.report.win.height >= 28 && !(await hiddenNow()), bd);
  await strip.evaluate(() => document.getElementById('pin').click());
  // the strip hides, then reports it: wait for main's layout too (the page below the zone)
  const unpinned = await waitFor(async () => {
    const b = await barInfo();
    return !b.layout.pinned && b.layout.top === 6 && (await hiddenNow()) ? b : null;
  }, 9000);
  check('unpinned, the bar hides to its hint line: the page starts below the 6 px hit zone', unpinned && unpinned.pins.floating === false, unpinned || (await barInfo()));
  const hint = await strip.evaluate(() => Math.round(document.getElementById('hint').getBoundingClientRect().height));
  check('the hint line is 2 px', hint === 2, hint);
  await strip.mouse.move(300, 3);
  await sleep(60);
  await strip.mouse.move(300, 400);
  await sleep(250);
  check('a pass through the hit zone shorter than the dwell does not reveal it', await hiddenNow());
  await strip.mouse.move(300, 3);
  const hovered = await waitFor(async () => !(await hiddenNow()) || null, 1500, 30);
  bd = await barInfo();
  check('a dwell in the hit zone reveals it (floating: the page moves below it)', hovered && bd.report.win.shown && bd.layout.top === bd.report.win.height, bd);
  await strip.mouse.move(300, 400);
  await sleep(150);
  check('it stays through the grace after the pointer leaves', !(await hiddenNow()));
  check('and hides after it', await waitFor(hiddenNow, 2000, 30));
  await strip.mouse.move(300, 3);
  await waitFor(async () => !(await hiddenNow()) || null, 1500, 30);
  await strip.evaluate(() => document.getElementById('url').focus());
  await strip.mouse.move(300, 400);
  await sleep(700);
  check('the focused URL field holds it open after the pointer leaves', !(await hiddenNow()));
  await strip.evaluate(() => document.getElementById('url').blur());
  check('blurred, it hides again', await waitFor(hiddenNow, 2000, 30));
  await strip.evaluate((u) => window.stripBridge.action('navigate', u), `127.0.0.1:${webPort}/pulse`);
  check('a navigation shows the hidden bar for a moment', await waitFor(async () => !(await hiddenNow()) || null, 3000, 30));
  check('then it hides again', await waitFor(hiddenNow, 4000, 50));
  // ⌘⇧S (main clicks #send) with the bar hidden: the note bar shows it, then takes the keyboard
  await strip.evaluate(() => document.getElementById('send').click());
  const noted = await strip.evaluate(() => ({
    focused: document.activeElement && document.activeElement.id,
    hidden: document.documentElement.classList.contains('bar-hidden'),
  }));
  check('send with the bar hidden: the bar shows and the note field has the keyboard', noted.focused === 'note' && !noted.hidden, noted);
  await strip.evaluate(() => document.getElementById('sendcancel').click());
  check('cancelled, it hides again', await waitFor(hiddenNow, 2000, 30));
  // a status message (here: sent) shows in the bar: it holds the hidden bar open until it goes
  await strip.evaluate(() => window.stripBridge.action('send_page', { note: 'held' }));
  const held = await waitFor(async () => {
    const v = await strip.evaluate(() => ({ status: !document.getElementById('status').hidden, hidden: document.documentElement.classList.contains('bar-hidden') }));
    return v.status && !v.hidden ? v : null;
  }, 3000, 50);
  check('a status message holds the hidden bar open', !!held, held);
  check('and it hides once the message goes', await waitFor(hiddenNow, 9000, 100));
  // the pin is per surface: docked keeps its own (unpinned by default)
  await ctl(`__herdrCompanion.strip_preview({dock: true, kind: 'side', features: ['placements', 'placement_actions']})`);
  const dockPin = await waitFor(async () => {
    const v = await strip.evaluate(() => document.getElementById('pin').getAttribute('aria-pressed'));
    return v === 'false' ? v : null;
  }, 2000);
  check('docked, the bar is unpinned by default', dockPin === 'false', dockPin);
  await strip.evaluate(() => document.getElementById('pin').click());
  const saved = await waitFor(async () => {
    const b = await barInfo();
    return b.pins.docked === true ? b.pins : null;
  }, 2000);
  check('pinning it docked saves the docked pin only', saved && saved.docked === true && saved.floating === false, saved);
  const file = JSON.parse(fs.readFileSync(path.join(profile, 'bar.json'), 'utf8'));
  check('the pins live in the profile (bar.json)', file.pinned && file.pinned.docked === true && file.pinned.floating === false, file);
  await ctl('__herdrCompanion.strip_preview(null)');
  // pinned again for the checks below (the floating pin is checked again after a restart)
  await strip.evaluate(() => document.getElementById('pin').click());
  check('pinned again, it stays', await waitFor(async () => (await barInfo()).pins.floating === true && !(await hiddenNow()), 2000));
  // the update chip (herdr installed newer app files than this process runs): disabled only
  // while its restart request runs; a failed request brings it back
  const chip = () =>
    strip.evaluate(() => {
      const u = document.getElementById('update');
      return { hidden: u.hidden, disabled: u.disabled, status: document.getElementById('status').textContent };
    });
  mock.state.update = '9.9.9';
  mock.state.companionDelayMs = 1000;
  check('the lease answer\'s update shows the chip', await waitFor(async () => !(await chip()).hidden, 5000), await chip());
  await strip.evaluate(() => document.getElementById('update').click());
  const busy = await waitFor(async () => ((await chip()).disabled ? chip() : null), 800, 50);
  check('the chip is disabled while its restart runs', busy, busy);
  await strip.evaluate(() => document.getElementById('update').click()); // one request at a time
  const failedChip = await waitFor(async () => {
    const c = await chip();
    return !c.hidden && !c.disabled && /Restart failed/.test(c.status) ? c : null;
  }, 4000);
  check('a failed restart leaves the chip clickable again', failedChip, await chip());
  check('the chip asked herdr once', mock.state.companionCalls.filter((c) => c.action === 'restart').length === 1, mock.state.companionCalls);
  mock.state.update = null;
  mock.state.companionDelayMs = 0;
  check('the chip goes with the update', await waitFor(async () => (await chip()).hidden, 5000), await chip());
  // web page permissions are never auto-granted: asked in the strip (deny / allow once)
  const permBar = () =>
    strip.evaluate(() => {
      const b = document.getElementById('permbar');
      return !b.hidden && b.dataset.id ? { id: b.dataset.id, text: document.getElementById('permtext').textContent } : null;
    });
  const wv = (await ctl('__herdrCompanion.views()')).find((v) => v.tab_id === 'w1:t2' && v.active);
  const wp = wv && (await waitFor(() => pageForTarget(wv.target_id), 3000));
  const denyP = wp.evaluate(() => Notification.requestPermission());
  const bar1 = await waitFor(permBar, 3000);
  check('a notification request is not granted but asked in the strip', bar1 && /show notifications/.test(bar1.text), bar1);
  await strip.evaluate(() => document.getElementById('permdeny').click());
  const denied = await Promise.race([denyP, sleep(3000).then(() => 'timeout')]);
  check('Deny answers the page with a refusal and hides the bar', denied !== 'granted' && denied !== 'timeout' && !(await permBar()), denied);
  const allowP = wp.evaluate(
    () => new Promise((r) => navigator.geolocation.getCurrentPosition(() => r('position'), (e) => r(e.code === 1 ? 'denied' : `error ${e.code}`), { timeout: 3000 })),
  );
  const bar2 = await waitFor(permBar, 3000);
  check('a geolocation request is asked in the strip', bar2 && /know your location/.test(bar2.text), bar2);
  await strip.evaluate(() => document.getElementById('permallow').click());
  const geo = await Promise.race([allowP, sleep(6000).then(() => 'timeout')]);
  check('Allow once lets that request through (not denied)', geo !== 'denied' && geo !== 'timeout', geo);
  // a microphone grant is not a camera grant
  const media = (c) => wp.evaluate((c) => navigator.mediaDevices.getUserMedia(c).then((st) => { st.getTracks().forEach((t) => t.stop()); return 'granted'; }, (e) => e.name), c);
  const micP = media({ audio: true });
  const bar3 = await waitFor(permBar, 3000);
  check('a microphone request is asked in the strip', bar3 && /microphone/.test(bar3.text), bar3);
  await strip.evaluate(() => document.getElementById('permallow').click());
  const mic = await Promise.race([micP, sleep(6000).then(() => 'timeout')]);
  check('Allow once lets the microphone through', mic !== 'NotAllowedError' && mic !== 'timeout', mic);
  const camP = media({ video: true });
  const bar4 = await waitFor(permBar, 3000);
  check('a camera request after a microphone grant is asked again', bar4 && /camera/.test(bar4.text), bar4);
  if (bar4) await strip.evaluate(() => document.getElementById('permdeny').click());
  const cam = await Promise.race([camP, sleep(6000).then(() => 'timeout')]);
  check('denying it refuses the camera', cam === 'NotAllowedError', cam);
  // a page cannot open a data: tab
  const nViews = (await ctl('__herdrCompanion.views()')).length;
  await wp.evaluate(() => window.open('data:text/html,<title>pop</title>pop'));
  await sleep(600);
  check('a data: popup opens no tab', (await ctl('__herdrCompanion.views()')).length === nViews);
  // selecting a tab in the strip is the user looking at it
  const sel = (await ctl('__herdrCompanion.views()')).find((v) => v.tab_id === 'w1:t2');
  const shownBefore = mock.state.reports.filter((r) => r.action === 'shown').length;
  await strip.evaluate((id) => window.stripBridge.action('select', id), sel.view_id);
  const shownSel = await waitFor(() => mock.state.reports.slice(0).filter((r) => r.action === 'shown').length > shownBefore && mock.state.reports.findLast((r) => r.action === 'shown'), 3000);
  check('a strip select reports shown (clears needs-you)', shownSel && shownSel.view_id === sel.view_id, shownSel);
  const before = (await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2').length;
  const active = (await ctl('__herdrCompanion.views()')).find((v) => v.tab_id === 'w1:t2' && v.active);
  await strip.evaluate((id) => window.stripBridge.action('close', id), active.view_id);
  const after = await waitFor(
    async () =>
      (await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2').length === before - 1 &&
      mock.state.reports.some((r) => r.action === 'close_tab')
        ? true
        : null,
    3000,
  );
  check('strip close removes the page and reports close_tab', after, {
    before,
    now: (await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2').map((v) => [v.view_id, v.url.slice(0, 40)]),
    closed: active && active.view_id,
  });
  // The user's click on the close button itself (DOM path, not the bridge): before the fix the
  // tab's mousedown select re-rendered the strip and the button's click never fired.
  {
    const left = (await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2');
    if (left.length > 0) {
      const n0 = left.length;
      // A real click with a human press length: hit testing decides where mouseup/click land,
      // so a strip re-rendered between press and release swallows the click (the bug).
      const box = await strip.evaluate(() => {
        const b = document.querySelector('.tab.active .close') || document.querySelector('.tab .close');
        if (!b) return null;
        const r = b.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
      });
      let clicked = false;
      if (box) {
        await strip.mouse.move(box.x, box.y);
        await strip.mouse.down();
        await new Promise((r) => setTimeout(r, 150));
        await strip.mouse.up();
        clicked = true;
      }
      const gone = await waitFor(async () => ((await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2').length === n0 - 1 ? true : null), 3000);
      check('a click on a tab close button closes that tab (DOM path)', clicked && gone, { clicked, n0 });
    }
  }

  // html view sandbox
  const site = path.join(tmp, 'site');
  fs.mkdirSync(site);
  fs.writeFileSync(path.join(tmp, 'outside.html'), '<body>OUTSIDE</body>');
  fs.writeFileSync(path.join(site, 'inside.html'), '<body>INSIDE</body>');
  fs.symlinkSync(path.join(tmp, 'outside.html'), path.join(site, 'link.html'));
  fs.writeFileSync(
    path.join(site, 'index.html'),
    `<title>report</title><iframe id=a src="inside.html"></iframe><iframe id=b src="../outside.html"></iframe><iframe id=c src="link.html"></iframe>`,
  );
  const hitsBefore = hits;
  const sh = await ctl(`__herdrCompanion.show({tab_id:'w1:t2', file:${JSON.stringify(path.join(site, 'index.html'))}, focus:true})`);
  check('show opens an html view', sh && sh.view_id && sh.target_id, sh);
  const hp = await waitFor(() => pageForTarget(sh.target_id), 5000);
  await hp.waitForLoadState('load').catch(() => {});
  await sleep(500);
  const frameText = async (name) => {
    const f = hp.frames().find((fr) => fr.name() === name || fr.url().endsWith(name));
    if (!f) return null;
    return f.evaluate(() => document.body && document.body.innerText).catch(() => null);
  };
  const fa = await frameText('inside.html');
  const fb = await frameText('outside.html');
  const fc = await frameText('link.html');
  check('html view reads files inside its directory', fa === 'INSIDE', fa);
  check('html view cannot read outside its directory or through a symlink', fb !== 'OUTSIDE' && fc !== 'OUTSIDE', { fb, fc });
  const netRes = await hp.evaluate((u) => fetch(u).then(() => 'ok', () => 'blocked'), `http://127.0.0.1:${webPort}/n`);
  check('html view network is blocked by default', netRes === 'blocked' && hits === hitsBefore, { netRes, hits: hits - hitsBefore });
  const rtc = async (pg) => [await pg.evaluate(() => typeof RTCPeerConnection), ...(await Promise.all(pg.frames().slice(1).map((f) => f.evaluate(() => typeof RTCPeerConnection).catch((e) => e.message))))];
  const rtcNoNet = await rtc(hp);
  check('a no-network html view has no WebRTC (page and frames)', rtcNoNet.every((t) => t === 'undefined'), rtcNoNet);
  const shn = await ctl(`__herdrCompanion.show({tab_id:'w1:t2', file:${JSON.stringify(path.join(site, 'inside.html'))}, allow_network:true})`);
  const hpn = await waitFor(() => pageForTarget(shn.target_id), 5000);
  await hpn.waitForLoadState('load').catch(() => {});
  const netOk = await hpn.evaluate((u) => fetch(u, { mode: 'no-cors' }).then(() => 'ok', (e) => String(e)), `http://127.0.0.1:${webPort}/n2`);
  check('allow_network lets an html view reach the network', netOk === 'ok', netOk);
  check('a network-allowed html view keeps WebRTC', (await hpn.evaluate(() => typeof RTCPeerConnection)) === 'function');
  // an explicit title stays; a view's script navigation opens a tab on the agent's behalf
  fs.writeFileSync(path.join(site, 'nav.html'), `<title>page title</title><script>setTimeout(() => { location.href = 'http://127.0.0.1:${webPort}/fromview'; }, 300)</script>`);
  const newTabsBefore = mock.state.reports.filter((r) => r.action === 'new_tab').length;
  const shf = await ctl(`__herdrCompanion.show({tab_id:'w1:t2', file:${JSON.stringify(path.join(site, 'nav.html'))}, title:'Fixed', opened_by:'agent:w1:p9'})`);
  const fromView = await waitFor(() => mock.state.tabs.get('w1:t2').browser.entries.find((e) => e.url.includes('/fromview')), 6000);
  check('a view\'s script navigation opens a web tab for the view\'s opener', fromView && fromView.opened_by === 'agent:w1:p9', fromView);
  check('it is not reported as a user new_tab', mock.state.reports.filter((r) => r.action === 'new_tab').length === newTabsBefore);
  const fixed = mock.state.tabs.get('w1:t2').browser.entries.find((e) => e.view_id === shf.view_id);
  check('an explicit --title is not replaced by the page title', fixed && fixed.title === 'Fixed', fixed);
  const missing = await ctl(`__herdrCompanion.show({tab_id:'w1:t2', file:'/nonexistent/x.html'})`);
  check('show of a missing file answers file_not_found', missing && missing.error && missing.error.code === 'file_not_found', missing);
  const synced = await waitFor(() => mock.state.tabs.get('w1:t2').browser.entries.find((e) => e.kind === 'html_view' && e.file), 3000);
  check('html views sync kind, file and allow_network', synced, synced);

  // a page cannot claim a window drag region (it would take the mouse from the dock's bar);
  // the companion's own strip keeps its drag region
  {
    const regions = async (pg) =>
      Promise.all(
        pg.frames().map((f) =>
          f
            .evaluate(() => {
              const d = document.getElementById('d');
              return d ? [getComputedStyle(d).appRegion, getComputedStyle(d, '::before').appRegion, getComputedStyle(d).webkitAppRegion] : null;
            })
            .catch((e) => e.message),
        ),
      );
    const two = async (pg) => ((x) => (x.filter(Boolean).length === 2 ? x : null))(await regions(pg));
    const allNoDrag = (rs) => rs.filter(Boolean).length === 2 && rs.filter(Boolean).every((r) => Array.isArray(r) && r.every((v) => v === 'no-drag'));
    const od = await ctl(`__herdrCompanion.open({tab_id:'w1:t2', url:'http://127.0.0.1:${webPort}/drag1', background:false})`);
    const dp = await waitFor(() => pageForTarget(od.target_id), 5000);
    const r1 = await waitFor(() => two(dp), 5000);
    check('a web page and its cross-site frame get no drag region (drag !important, ::before)', r1 && allNoDrag(r1), r1);
    const agentsBefore = webAgents.length;
    await dp.evaluate((u) => {
      location.href = u;
    }, `http://127.0.0.1:${webPort}/drag2`);
    const r2 = await waitFor(async () => (dp.url().endsWith('/drag2') ? two(dp) : null), 5000);
    check('… nor after navigating to another drag page', r2 && allNoDrag(r2), r2);
    const ua = await dp.evaluate(() => navigator.userAgent);
    const seen = webAgents.slice(agentsBefore);
    check(
      "a web page's user agent has no Electron/ token (navigator and request header)",
      !/Electron\//.test(ua) && /Chrome\//.test(ua) && seen.length > 0 && seen.every((a) => a && !/Electron\//.test(a)),
      { ua, seen },
    );
    await ctl(`__herdrCompanion.close(${JSON.stringify(od.view_id)})`);
    fs.writeFileSync(path.join(site, 'drag.html'), `<title>drag</title>${DRAG_CSS}<div id=d>d</div><iframe src="dragframe.html"></iframe>`);
    fs.writeFileSync(path.join(site, 'dragframe.html'), `${DRAG_CSS}<div id=d>f</div>`);
    const sd = await ctl(`__herdrCompanion.show({tab_id:'w1:t2', file:${JSON.stringify(path.join(site, 'drag.html'))}})`);
    const sp = await waitFor(() => pageForTarget(sd.target_id), 5000);
    const r3 = await waitFor(() => two(sp), 5000);
    check('an html view and its frame get no drag region', r3 && allNoDrag(r3), r3);
    const hua = await sp.evaluate(() => navigator.userAgent);
    check("an html view's user agent has no Electron/ token", !/Electron\//.test(hua), hua);
    await ctl(`__herdrCompanion.close(${JSON.stringify(sd.view_id)})`);
    const strips = mock.state.browser.contexts().flatMap((c) => c.pages()).filter((p) => p.url().startsWith('herdr-companion://ui/strip.html'));
    const own = await Promise.all(
      strips.map((p) =>
        p.evaluate(() => ({ docked: document.body.classList.contains('docked'), bar: getComputedStyle(document.getElementById('bar')).appRegion })),
      ),
    );
    // strip.css: the floating bar drags its window; the docked one has no drag region
    check(
      'the strip keeps its own drag region (floating: drag, docked: no-drag)',
      own.length > 0 && own.some((o) => !o.docked) && own.every((o) => o.bar === (o.docked ? 'no-drag' : 'drag')),
      own,
    );
  }

  // an agent focus() into a non-focused herdr tab survives unrelated tab events
  const fo = await ctl(`__herdrCompanion.show({tab_id:'w1:t1', file:${JSON.stringify(path.join(site, 'inside.html'))}, focus:true})`);
  await api('mock.create_tab', { label: 'later' });
  await sleep(800);
  const stillShown = (await ctl('__herdrCompanion.views()')).find((v) => v.view_id === fo.view_id);
  check('an agent-focused page stays shown across unrelated tab events', stillShown && stillShown.shown, stillShown);
  const t3 = await api('mock.create_tab', { label: 'fresh' });
  const fresh = await ctl(`__herdrCompanion.open({tab_id:${JSON.stringify(t3.tab_id)}, url:'about:blank'})`);
  check('open into a herdr tab created a moment ago works', fresh && fresh.view_id && !fresh.error, fresh);

  // server restart / live handoff: same instance re-registers, full resync from seq 1
  const syncsBefore = mock.state.syncs.length;
  await api('mock.restart');
  const relet = await waitFor(() => (mock.state.lease && mock.state.registers.length >= 2 ? mock.state.lease : null), 20000);
  check('re-registers after a server restart with the same instance', relet && relet.instance === instance1, relet && { instance: relet.instance });
  const resync = await waitFor(() => mock.state.syncs.slice(syncsBefore).find((s) => s.seq === 1 && s.groups.length >= 2), 5000);
  check('the new lease gets a full resync (seq 1, every group, targets kept)', resync && resync.groups.every((g) => g.entries.every((e) => e.target)), resync && resync.groups.map((g) => g.tab_id));

  // tab.closed drops the group after the grace period
  await api('mock.close_tab', { tab_id: 'w1:t2' });
  const gone = await waitFor(async () => ((await ctl('__herdrCompanion.views()')).every((v) => v.tab_id !== 'w1:t2') ? true : null), 4000);
  check('tab.closed destroys the group', gone);

  // restart of the companion restores the session's pages from herdr's state
  await api('mock.focus_tab', { tab_id: 'w1:t1' });
  await sleep(300);
  // and keeps the bar's pins: the floating one unpinned now
  await stripPage().evaluate(() => document.getElementById('pin').click());
  await waitFor(async () => (await ctl('__herdrCompanion.bar_debug()')).pins.floating === false, 2000);
  const quit1 = await quitCompanion();
  check('quits through its own control path', quit1);
  check('unregistered on quit', !mock.state.lease);
  if (quit1) {
    startCompanion();
    const lease2 = await waitFor(() => mock.state.lease, 40000);
    check('a new instance registers', lease2 && lease2.instance !== instance1);
    const restored = await waitFor(async () => {
      const v = await ctl('__herdrCompanion.views()');
      const t1 = v.filter((x) => x.tab_id === 'w1:t1');
      const act = t1.find((x) => x.active);
      return t1.length === 2 && t1.every((x) => x.id) && act && act.live && act.shown && act.target_id ? act : null;
    }, 8000);
    check('the focused session is restored (entry ids kept, active page live and shown)', restored, restored);
    const rsync = await waitFor(() => {
      const e = mock.state.tabs.get('w1:t1').browser.entries.find((x) => restored && x.id === restored.id);
      return e && e.target === restored.target_id ? e : null;
    }, 4000);
    check('the restored view syncs its new target', rsync);
    const pins = await ctl('__herdrCompanion.bar_debug()');
    check('the bar\'s pins survive a restart (floating unpinned, docked pinned)', pins && pins.pins.floating === false && pins.pins.docked === true && pins.layout && !pins.layout.pinned, pins);
    check('quits again', await quitCompanion());
  }
} catch (e) {
  check('no exception', false, e.stack);
  await quitCompanion();
} finally {
  await mock.close();
  web.close();
  dlgWeb.close();
  console.log(`\n${results.length - failed}/${results.length} passed; log ${logFile}`);
  if (!failed) fs.rmSync(tmp, { recursive: true, force: true });
  process.exit(failed ? 1 : 0);
}
