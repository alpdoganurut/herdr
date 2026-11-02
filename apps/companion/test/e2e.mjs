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

const mock = createMockHerdr({ socketPath: sock, verifyCdp: true, heartbeatMs: 1000, ttlMs: 3000 });
await mock.listen();

let client;
async function api(method, params) {
  if (!client || client.closed) {
    client = new Connection(sock, { prefix: 't' });
    await client.connect();
  }
  return client.request(method, params);
}
const ctl = async (expr) => (await api('mock.control', { expression: expr })).value;

let child = null;
const logFile = path.join(tmp, 'companion.log');
function startCompanion() {
  const out = fs.openSync(logFile, 'a');
  child = spawn(electronBin, [appDir, '--socket', sock, '--profile-dir', profile, '--headless'], {
    env: { ...process.env, HERDR_COMPANION_HEADLESS: '1', HERDR_SOCKET_PATH: '' },
    stdio: ['ignore', out, out],
  });
  child.exited = new Promise((r) => child.on('exit', (code) => r(code)));
}
async function quitCompanion() {
  if (!child) return true;
  try {
    await ctl('__herdrCompanion.quit()');
  } catch {}
  const code = await Promise.race([child.exited, sleep(10000).then(() => 'timeout')]);
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
const stripPage = () => mock.state.browser.contexts()[0].pages().find((p) => p.url().startsWith('herdr-companion://ui/'));

// a local http server to observe html-view network blocking
let hits = 0;
const web = http.createServer((req, res) => {
  hits++;
  res.end('<title>web</title>web');
});
await new Promise((r) => web.listen(0, '127.0.0.1', r));
const webPort = web.address().port;

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

  // strip actions through the strip page (same IPC the buttons use)
  const strip = stripPage();
  check('strip page is a herdr-companion: furniture page', !!strip);
  await strip.evaluate(() => window.stripBridge.action('new_tab'));
  await strip.evaluate((u) => window.stripBridge.action('navigate', u), `127.0.0.1:${webPort}/x`);
  const nav = await waitFor(() => mock.state.reports.find((r) => r.action === 'navigate' && r.url.includes(String(webPort))), 3000);
  check('URL box navigate is reported as a user action', nav);
  const navSynced = await waitFor(() => mock.state.tabs.get('w1:t2').browser.entries.find((e) => e.url.includes(`:${webPort}/x`) && e.title === 'web'), 5000);
  check('the navigated page syncs url and title', navSynced);
  await strip.evaluate(() => window.stripBridge.action('send_page', { note: 'look' }));
  const sent = await waitFor(() => mock.state.sent[0], 3000);
  check('send to agent calls browser.send_page with tab, url and note', sent && sent.tab_id === 'w1:t2' && sent.note === 'look' && sent.url.includes(String(webPort)), sent);
  const before = (await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2').length;
  const active = (await ctl('__herdrCompanion.views()')).find((v) => v.tab_id === 'w1:t2' && v.active);
  await strip.evaluate((id) => window.stripBridge.action('close', id), active.view_id);
  const after = await waitFor(async () => ((await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2').length === before - 1 ? true : null), 3000);
  check('strip close removes the page and reports close_tab', after && mock.state.reports.some((r) => r.action === 'close_tab'), {
    before,
    now: (await ctl('__herdrCompanion.views()')).filter((v) => v.tab_id === 'w1:t2').map((v) => [v.view_id, v.url.slice(0, 40)]),
    closed: active && active.view_id,
  });

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
  const shn = await ctl(`__herdrCompanion.show({tab_id:'w1:t2', file:${JSON.stringify(path.join(site, 'inside.html'))}, allow_network:true})`);
  const hpn = await waitFor(() => pageForTarget(shn.target_id), 5000);
  await hpn.waitForLoadState('load').catch(() => {});
  const netOk = await hpn.evaluate((u) => fetch(u, { mode: 'no-cors' }).then(() => 'ok', (e) => String(e)), `http://127.0.0.1:${webPort}/n2`);
  check('allow_network lets an html view reach the network', netOk === 'ok', netOk);
  const missing = await ctl(`__herdrCompanion.show({tab_id:'w1:t2', file:'/nonexistent/x.html'})`);
  check('show of a missing file answers file_not_found', missing && missing.error && missing.error.code === 'file_not_found', missing);
  const synced = await waitFor(() => mock.state.tabs.get('w1:t2').browser.entries.find((e) => e.kind === 'html_view' && e.file), 3000);
  check('html views sync kind, file and allow_network', synced, synced);

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
  client = null;
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
    check('quits again', await quitCompanion());
  }
} catch (e) {
  check('no exception', false, e.stack);
  await quitCompanion();
} finally {
  if (client) client.close();
  await mock.close();
  web.close();
  console.log(`\n${results.length - failed}/${results.length} passed; log ${logFile}`);
  if (!failed) fs.rmSync(tmp, { recursive: true, force: true });
  process.exit(failed ? 1 : 0);
}
