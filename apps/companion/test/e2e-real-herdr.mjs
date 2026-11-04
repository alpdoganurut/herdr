// Driver of test/e2e-real-herdr.sh (run that, not this): the companion against a real, isolated
// herdr. Expects HERDR_E2E_TMP / HERDR_E2E_BIN / HERDR_E2E_SOCKET and an isolated environment.
import { spawn, execFile } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const electronBin = require('electron');
const { chromium } = require('playwright-core');
const { Connection } = require('../src/herdr-api.js');
const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const T = process.env.HERDR_E2E_TMP;
const BIN = process.env.HERDR_E2E_BIN;
const SOCK = process.env.HERDR_E2E_SOCKET;
if (!T || !BIN || !SOCK) {
  console.error('run test/e2e-real-herdr.sh');
  process.exit(2);
}
const PROFILE = path.join(T, 'companion-profile');

const results = [];
let failed = 0;
const check = (name, ok, detail) => {
  results.push({ name, ok: !!ok });
  if (!ok) failed++;
  const d = detail === undefined ? '' : ' — ' + (typeof detail === 'string' ? detail : JSON.stringify(detail)).slice(0, 400);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${d}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// Every step has a deadline: a stuck step fails fast instead of blocking the run.
const within = (p, ms, what) => {
  let timer;
  return Promise.race([
    Promise.resolve(p).finally(() => clearTimeout(timer)),
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${what} timed out after ${ms} ms`)), ms);
    }),
  ]);
};
const SECTION_MS = 4 * 60 * 1000;
const RUN_MS = 25 * 60 * 1000;
async function waitFor(fn, ms = 10000, step = 200) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const v = await within(fn(), Math.max(1000, end - Date.now()), 'a waitFor probe');
      if (v) return v;
    } catch {}
    await sleep(step);
  }
  return null;
}
async function section(name, fn) {
  console.log(`\n== ${name}`);
  try {
    await within(fn(), SECTION_MS, `section "${name}"`);
  } catch (e) {
    check(`${name}: no exception`, false, e && e.stack ? e.stack : String(e));
  }
}

// ---------- herdr: JSON API (one request per connection) and the isolated CLI ----------

async function api(method, params = {}, timeoutMs = 30000) {
  const c = new Connection(SOCK, { prefix: 'e2e', timeoutMs });
  c.on('error', () => {});
  await c.connect();
  try {
    return await c.request(method, params, { timeoutMs });
  } finally {
    c.close();
  }
}
const apiOk = (method, params, ms) => api(method, params, ms).then((r) => r, (e) => ({ error: { code: e.code, message: e.message } }));

// `herdr browser <args> --json` as an agent in `pane`: {result} or {error}.
function browser(args, pane, timeoutMs = 60000) {
  const argv = ['browser', ...args, '--json', ...(pane ? ['--pane', pane] : [])];
  return new Promise((resolve) => {
    execFile(BIN, argv, { timeout: timeoutMs, maxBuffer: 16 << 20 }, (err, stdout, stderr) => {
      // --json: a success on stdout, an error response on stderr (one JSON line each)
      let v = null;
      for (const line of `${stdout}\n${stderr}`.split('\n')) {
        try {
          const x = JSON.parse(line);
          if (x && (x.result || x.error)) {
            v = x;
            break;
          }
        } catch {}
      }
      if (v && v.error) return resolve({ error: v.error });
      if (v && v.result) return resolve({ result: v.result.result || v.result });
      resolve({ error: { code: 'cli', message: `${err ? err.message : ''} ${stderr} ${stdout}`.trim().slice(0, 400) } });
    });
  });
}
const text = (r) => (r && r.result ? `${r.result.header || ''}\n${r.result.text || ''}\n${JSON.stringify(r.result.data || {})}` : '');

// Starts `herdr server` once the previous one is gone (a second one refuses while it lives).
async function startServer() {
  for (let attempt = 0; attempt < 5; attempt++) {
    const out = fs.openSync(path.join(T, 'server.log'), 'a');
    const p = spawn(BIN, ['server'], { stdio: ['ignore', out, out] });
    fs.appendFileSync(path.join(T, 'server.pids'), `${p.pid}\n`);
    const exited = new Promise((r) => p.on('exit', () => r('exited')));
    const up = await Promise.race([waitFor(() => fs.existsSync(SOCK) && api('browser.get', {}).then(() => true), 20000), exited]);
    if (up === true) return true;
    await sleep(2000);
  }
  return false;
}

// ---------- a local web server ----------

let hits = 0;
const web = http.createServer((req, res) => {
  hits++;
  const pages = {
    '/a': '<title>page a</title><h1>A</h1>',
    '/b': '<title>page b</title><button id=btn onclick="document.getElementById(\'out\').textContent=\'clicked\'">go</button><p id=out>idle</p>',
    '/c': '<title>page c</title><p>C</p>',
    '/d': '<title>page d</title><p>D</p>',
    '/e': '<title>page e</title><p>E</p>',
    '/boom': '<title>boom</title><p>boom</p><script>setTimeout(() => { throw new Error("e2e boom " + location.pathname) }, 100)</script>',
  };
  res.setHeader('content-type', 'text/html');
  res.end(pages[req.url.split('?')[0]] || '<title>other</title>other');
});
await new Promise((r) => web.listen(0, '127.0.0.1', r));
const W = `http://127.0.0.1:${web.address().port}`;

// ---------- the companion (headless Electron) and an inspector on its CDP port ----------

let child = null;
const SYNC_BUDGET = 1500;
let insp = null; // {browser, control(), strip()}
function startCompanion() {
  const out = fs.openSync(path.join(T, 'companion.log'), 'a');
  child = spawn(electronBin, [appDir, '--socket', SOCK, '--profile-dir', PROFILE, '--headless'], {
    // A small sync budget: the companion splits its syncs (one group each, titles cut when one
    // group alone is over it), as it does for a real oversized sync.
    env: { ...process.env, HERDR_COMPANION_HEADLESS: '1', HERDR_COMPANION_SYNC_BUDGET: String(SYNC_BUDGET) },
    stdio: ['ignore', out, out],
  });
  child.exited = new Promise((r) => child.on('exit', (code) => r(code)));
}
async function inspector() {
  if (insp) await within(insp.browser.close(), 10000, 'inspector close').catch(() => {});
  const port = await waitFor(() => Number(fs.readFileSync(path.join(PROFILE, 'DevToolsActivePort'), 'utf8').split('\n')[0]) || null, 20000);
  const b = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 20000 });
  const find = (prefix) => b.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith(prefix));
  insp = { browser: b, control: () => find('herdr-companion://control/'), strip: () => find('herdr-companion://ui/strip.html') };
  return insp;
}
const views = () => within(insp.control().evaluate(() => globalThis.__herdrCompanion.views()), 10000, 'views()');
const stripStatus = () =>
  within(insp.strip().evaluate(() => (document.getElementById('status').hidden ? '' : document.getElementById('status').textContent)), 10000, 'strip status');
const stripAction = (name, arg) => within(insp.strip().evaluate(([n, a]) => window.stripBridge.action(n, a), [name, arg === undefined ? null : arg]), 10000, `strip ${name}`);
async function quitCompanion() {
  if (!child) return true;
  try {
    await within(insp.control().evaluate(() => globalThis.__herdrCompanion.quit()), 10000, 'quit()');
  } catch {}
  await within(insp.browser.close(), 10000, 'inspector close').catch(() => {});
  insp = null;
  const code = await Promise.race([child.exited, sleep(30000).then(() => 'timeout')]);
  if (code === 'timeout') {
    console.log(`companion did not exit; abandoned pid ${child.pid}`);
    return false;
  }
  child = null;
  return true;
}
const profileState = async (name) => {
  const info = (await api('browser.get', {})).browser || {};
  const p = (info.profiles || []).find((x) => x.name === name);
  return p ? p.state : null;
};
const sessions = async () => ((await api('browser.get', {})).browser || {}).sessions || [];
// The lease is live, not just a pending registration (which already reads "running"):
// the companion has logged a new lease since `since` (a count of earlier lease lines) and
// every herdr tab with pages reports host "companion".
const leaseLines = () => (fs.existsSync(path.join(T, 'companion.log')) ? fs.readFileSync(path.join(T, 'companion.log'), 'utf8').match(/\] lease L/g) || [] : []).length;
let leasesSeen = 0;
const companionLive = async ({ newLease = true } = {}) => {
  if (newLease && leaseLines() <= leasesSeen) return false;
  const s = await sessions();
  if (!((await profileState('companion')) === 'running' && s.every((x) => x.host === 'companion'))) return false;
  leasesSeen = leaseLines();
  return true;
};

// ---------- the session under test ----------

// The whole run's deadline: report, quit the companion through its control page, exit.
setTimeout(async () => {
  console.log(`FAIL the run did not finish within ${RUN_MS / 60000} min`);
  if (child) await quitCompanion().catch(() => {});
  process.exit(1);
}, RUN_MS).unref();

let ws, t1, t2, p1, p2;
const panesOf = async () => (await api('pane.list', { workspace_id: ws })).panes || [];

try {
  await section('setup: an isolated herdr session, a Claude-kind pane', async () => {
    const w = await api('workspace.create', { label: 'e2e', focus: true });
    ws = (w.workspace && w.workspace.workspace_id) || w.workspace_id;
    const tabs = (await api('tab.list', { workspace_id: ws })).tabs || [];
    t1 = tabs[0].tab_id;
    const c = await api('tab.create', { workspace_id: ws, label: 'second', focus: false });
    t2 = (c.tab && c.tab.tab_id) || c.tab_id;
    const panes = await panesOf();
    p1 = panes.find((p) => p.tab_id === t1).pane_id;
    p2 = panes.find((p) => p.tab_id === t2).pane_id;
    await api('tab.focus', { tab_id: t1 });
    check('workspace with two tabs and their panes', ws && t1 && t2 && p1 && p2, { ws, t1, t2, p1, p2 });
    // herdr identifies agents by argv0: a sleep started as `claude` is a Claude-kind pane.
    await api('pane.send_text', { pane_id: p1, text: 'exec -a claude /bin/sleep 100000\r' });
    const agent = await waitFor(async () => {
      const p = (await panesOf()).find((x) => x.pane_id === p1);
      return p && /claude/i.test(JSON.stringify(p.agent || p.agent_id || '')) ? p : null;
    }, 20000, 500);
    check(`pane ${p1} runs a Claude-kind agent (argv0 claude)`, agent, agent && agent.agent);
  });

  await section('registration', async () => {
    startCompanion();
    const running = await waitFor(companionLive, 90000, 500);
    check('the companion registers (lease live, profile running)', running, await profileState('companion').catch(() => null));
    await inspector();
    check('its control page and strip are up', insp.control() && insp.strip());
    const v = await waitFor(async () => (await views()).length === 0 || null, 5000);
    check('no pages yet', v !== null);
  });

  let viewA;
  await section('agent ops land in the companion view of the pane\'s tab', async () => {
    const o = await browser(['open', `${W}/a`], p1);
    check('open answers with data.host = companion', o.result && o.result.data && o.result.data.host === 'companion', o.result ? o.result.data : o.error);
    viewA = await waitFor(async () => (await views()).find((x) => x.url.startsWith(`${W}/a`)), 5000);
    check(`the view is in the group of ${t1} (the caller's herdr tab)`, viewA && viewA.tab_id === t1, viewA);
    const n = await browser(['navigate', `${W}/b`], p1);
    check('navigate', !n.error && /page b/.test(text(n)), n.error || text(n).slice(0, 120));
    const moved = await waitFor(async () => (await views()).find((x) => x.view_id === viewA.view_id && x.url.startsWith(`${W}/b`)), 5000);
    check('the same companion view navigated', moved);
    const c = await browser(['click', '--selector', '#btn'], p1);
    check('click', !c.error, c.error);
    const r = await browser(['read', '--format', 'text'], p1);
    check('read sees the click', /clicked/.test(text(r)), text(r).slice(0, 200));
    const s = await browser(['screenshot', '--out', path.join(T, 'shot.png')], p1);
    const size = fs.existsSync(path.join(T, 'shot.png')) ? fs.statSync(path.join(T, 'shot.png')).size : 0;
    check('screenshot of the (hidden-or-shown) view', !s.error && size > 1000, s.error || { bytes: size });
  });

  await section('page dialogs: an agent sees and answers alert / confirm / prompt', async () => {
    // a dialog blocks the page: arm it from a timer, so the eval and its after-checks finish first
    const arm = (expr) => browser(['eval', `setTimeout(() => { window.__r = JSON.stringify(${expr}); }, 1500); 'armed'`], p1);
    const result = async () => {
      const r = await browser(['eval', 'window.__r'], p1);
      return r.error ? null : text(r);
    };
    const blocked = async () => {
      const r = await browser(['read', '--format', 'text'], p1);
      return r.error && r.error.code === 'dialog_open' ? r.error : null;
    };
    let a = await arm(`prompt('Copy this:', 'the text')`);
    check('a page prompt is armed', !a.error, a.error);
    let b = await waitFor(blocked, 8000, 300);
    check('herdr sees the open prompt (ops answer dialog_open with its message)', b && /prompt/.test(b.message) && /Copy this:/.test(b.message), b);
    const tabsR = await browser(['tabs'], p1);
    check('tabs lists the dialog', /dialog/.test(text(tabsR)), text(tabsR).slice(0, 300));
    const card = await waitFor(async () => {
      const p = insp.browser.contexts().flatMap((c) => c.pages()).find((x) => x.url().startsWith('herdr-companion://ui/dialog.html'));
      return p && (await p.evaluate(() => !document.getElementById('card').hidden && document.getElementById('field').value));
    }, 5000);
    check('the companion shows its card with the default', card === 'the text', card);
    let d = await browser(['dialog', 'accept', 'x'], p1);
    check('herdr browser dialog accept "x" answers it', !d.error && /prompt/.test(text(d)), d.error || text(d).slice(0, 200));
    let got = await waitFor(async () => ((v) => (v && /"x"/.test(v) ? v : null))(await result()), 5000, 300);
    check('the page sees "x"', got, got);
    a = await arm(`prompt('again', 'dflt')`);
    await waitFor(blocked, 8000, 300);
    d = await browser(['dialog', 'accept'], p1);
    got = await waitFor(async () => ((v) => (v && /"dflt"/.test(v) ? v : null))(await result()), 5000, 300);
    check('accept without text returns the default', !d.error && got, d.error || got);
    a = await arm(`prompt('cancel me', 'dflt')`);
    await waitFor(blocked, 8000, 300);
    d = await browser(['dialog', 'dismiss'], p1);
    got = await waitFor(async () => ((v) => (v && /null/.test(v) ? v : null))(await result()), 5000, 300);
    check('dismiss returns null', !d.error && got, d.error || got);
    a = await arm(`confirm('sure?')`);
    b = await waitFor(blocked, 8000, 300);
    check('a confirm is seen', b && /confirm/.test(b.message), b);
    d = await browser(['dialog', 'accept'], p1);
    got = await waitFor(async () => ((v) => (v && /true/.test(v) ? v : null))(await result()), 5000, 300);
    check('accepting it returns true', !d.error && got, d.error || got);
    a = await arm(`(alert('hello'), 'after alert')`);
    b = await waitFor(blocked, 8000, 300);
    check('an alert is seen', b && /alert/.test(b.message), b);
    d = await browser(['dialog', 'accept'], p1);
    got = await waitFor(async () => ((v) => (v && /after alert/.test(v) ? v : null))(await result()), 5000, 300);
    check('accepting it lets the page go on', !d.error && got, d.error || got);
    const none = await browser(['dialog', 'accept'], p1);
    check('no dialog open: no_dialog', none.error && none.error.code === 'no_dialog', none.error || none.result);
    // the user answers in the card: herdr's dialog state follows
    a = await arm(`prompt('user answers', 'u')`);
    await waitFor(blocked, 8000, 300);
    const cp = insp.browser.contexts().flatMap((c) => c.pages()).find((x) => x.url().startsWith('herdr-companion://ui/dialog.html'));
    await within(cp.evaluate(() => document.getElementById('ok').click()), 5000, 'card OK');
    const free = await waitFor(async () => {
      const r = await browser(['read', '--format', 'text'], p1);
      return !r.error;
    }, 5000, 300);
    check('a dialog the user answered no longer blocks the agent', free);
    got = await result();
    check('… and the page got the user\'s answer', got && /"u"/.test(got), got);
  });

  await section('focus following (tab.focused)', async () => {
    const o = await browser(['open', `${W}/c`], p2);
    const vc = await waitFor(async () => (await views()).find((x) => x.url.startsWith(`${W}/c`)), 5000);
    check(`an agent in ${t2} opens into ${t2}'s group, not shown while ${t1} has focus`, !o.error && vc && vc.tab_id === t2 && !vc.shown, vc || o.error);
    await api('tab.focus', { tab_id: t2 });
    const s2 = await waitFor(async () => (await views()).find((x) => x.tab_id === t2 && x.shown), 5000);
    check(`focusing ${t2} in herdr shows its group`, s2);
    await api('tab.focus', { tab_id: t1 });
    const s1 = await waitFor(async () => (await views()).find((x) => x.tab_id === t1 && x.shown), 5000);
    check(`focusing ${t1} again shows ${t1}'s active page`, s1 && s1.view_id === viewA.view_id, s1);
  });

  await section('html view: herdr browser show (sandbox, reload on change)', async () => {
    const file = path.join(T, 'site', 'report.html');
    const html = (v) =>
      `<title>report ${v}</title><p id=v>version ${v}</p><p id=net>net: pending</p><script>fetch(${JSON.stringify(W + '/net')}).then(() => { document.getElementById('net').textContent = 'net: open' }, () => { document.getElementById('net').textContent = 'net: blocked' })</script>`;
    fs.writeFileSync(file, html('one'));
    const before = hits;
    const sh = await browser(['show', file], p1);
    const d = (sh.result && sh.result.data) || {};
    check('show answers kind html_view on the companion, watching', d.kind === 'html_view' && d.host === 'companion' && d.watching === true, sh.error || d);
    const hv = await waitFor(async () => (await views()).find((x) => x.kind === 'html_view'), 5000);
    check(`the html view is in ${t1}'s group`, hv && hv.tab_id === t1, hv);
    await sleep(800);
    const r1 = await browser(['read', '--format', 'text'], p1);
    check('it renders the file with the network blocked', /version one/.test(text(r1)) && /net: blocked/.test(text(r1)) && hits === before, { text: text(r1).slice(0, 160), hits: hits - before });
    fs.writeFileSync(file, html('two'));
    const r2 = await waitFor(async () => {
      const r = await browser(['read', '--format', 'text'], p1);
      return /version two/.test(text(r)) ? r : null;
    }, 8000, 500);
    check('it reloads when the file changes', r2);
  });

  await section('needs-you: set by an agent focus, cleared when the user focuses that herdr tab', async () => {
    const f = await browser(['focus'], p2); // p2's current tab: page c in t2; herdr focus is on t1
    check('an agent focus in a background herdr tab succeeds (no window raise headless)', !f.error, f.error);
    const marked = await waitFor(async () => (await sessions()).find((s) => s.tab_id === t2 && s.needs_you), 5000);
    check(`browser.get sessions marks ${t2} needs_you`, marked, await sessions());
    const shown = await waitFor(async () => (await views()).find((x) => x.tab_id === t2 && x.shown), 3000);
    check(`the companion shows ${t2}'s page for the user`, shown);
    await api('tab.focus', { tab_id: t2 });
    const cleared = await waitFor(async () => (await sessions()).find((s) => s.tab_id === t2 && !s.needs_you), 5000);
    check(`focusing ${t2} in herdr while the companion already shows it clears needs_you`, cleared, await sessions());
    await api('tab.focus', { tab_id: t1 });
    await waitFor(async () => (await views()).find((x) => x.tab_id === t1 && x.shown), 3000);
  });

  await section('user actions from the strip land in the ledger as the user', async () => {
    await stripAction('navigate', `${W}/d`);
    await stripAction('reload');
    const logged = await waitFor(async () => {
      const r = await browser(['log', '-n', '50'], null);
      const s = JSON.stringify(r);
      return /user:navigate/.test(s) && /user:reload/.test(s) ? s : null;
    }, 8000, 500);
    check('browser log has user:navigate and user:reload', logged, logged ? undefined : (await browser(['log', '-n', '10'], null)));
  });

  await section('send page to the agent (and queued under do not disturb)', async () => {
    await stripAction('send_page', { note: 'e2e note one' });
    const st1 = await waitFor(async () => {
      const s = await stripStatus();
      return /Sent to|Queued for|No agent|failed/.test(s) ? s : null;
    }, 8000);
    check('send page reaches the Claude pane (strip: Sent/Queued)', /Sent to|Queued for/.test(st1 || ''), st1);
    await sleep(6500); // the strip message fades after 6 s
    await api('tab.set_dnd', { tab_id: t1, dnd: true });
    await stripAction('send_page', { note: 'e2e note two' });
    const st2 = await waitFor(async () => {
      const s = await stripStatus();
      return /Queued for|Sent to|No agent|failed/.test(s) ? s : null;
    }, 8000);
    check('under do not disturb it is queued quietly (strip says why)', /Queued for .*DND/.test(st2 || ''), st2);
    const quiet = await waitFor(async () => {
      const q = await api('agents.take_quiet', { caller_pane: p1 });
      return /e2e note two/.test(JSON.stringify(q)) ? q : null;
    }, 5000, 500);
    check('the DND page is in the pane\'s quiet store, from "you (browser)", fenced as untrusted', quiet && /you \(browser\)/.test(JSON.stringify(quiet)) && /untrusted/.test(JSON.stringify(quiet)), quiet);
  });

  await section('page errors go quietly to the Claude pane, batched', async () => {
    const o = await browser(['open', `${W}/boom`], p1);
    check('the agent opens a page that throws', !o.error, o.error);
    await browser(['reload'], p1); // a second error of the same kind (deduped/batched)
    const q = await waitFor(async () => {
      const r = await api('agents.take_quiet', { caller_pane: p1 });
      return /e2e boom/.test(JSON.stringify(r)) ? r : null;
    }, 20000, 1000);
    check('a quiet message from "herdr browser" names the error', q && /herdr browser/.test(JSON.stringify(q)), q);
    const again = await api('agents.take_quiet', { caller_pane: p1 });
    check('the same error is not pushed twice (dedupe)', !/e2e boom/.test(JSON.stringify(again)), again);
    await api('tab.set_dnd', { tab_id: t1, dnd: false });
  });

  let groupT1;
  await section('companion quit: the window path takes over (routing only, no window)', async () => {
    groupT1 = (await views()).filter((x) => x.tab_id === t1).map((x) => ({ url: x.url, kind: x.kind, active: x.active }));
    check('companion quits through its control page', await quitCompanion());
    const gone = await waitFor(async () => (await profileState('companion')) !== 'running', 20000, 500);
    check('the lease goes (profile companion no longer running)', gone, await profileState('companion'));
    const o = await browser(['open', `${W}/e`], p1, 90000);
    const code = o.error && o.error.code;
    check(
      'an agent open now routes to the herdr+ window profile (its launch fails here by design: no Chromium)',
      code && code !== 'host_unavailable' && /browser_executable_missing|browser_start_failed|browser_launch_timeout|browser_unavailable/.test(code),
      o.error || o.result,
    );
  });

  await section('companion restart restores each tab\'s group', async () => {
    startCompanion();
    const running = await waitFor(companionLive, 90000, 500);
    check('a new companion registers (lease live)', running);
    await inspector();
    const restored = await waitFor(async () => {
      const v = (await views()).filter((x) => x.tab_id === t1);
      const act = v.find((x) => x.active);
      return v.length === groupT1.length && act && act.live && act.shown ? v : null;
    }, 15000);
    check(`${t1}'s pages come back (same count, active page live and shown)`, restored, { before: groupT1, now: restored || (await views()) });
    const urls = (restored || []).map((x) => x.url).sort();
    check('with the same urls', JSON.stringify(urls) === JSON.stringify(groupT1.map((x) => x.url).sort()), urls);
    const t2v = (await views()).filter((x) => x.tab_id === t2);
    check(`${t2}'s pages are known too (placeholders until shown)`, t2v.length >= 1, t2v);
  });

  await section('server restart: the companion re-registers, groups survive', async () => {
    const before = await sessions();
    const viewsBefore = (await views()).map((x) => x.view_id).sort();
    await apiOk('server.stop', {});
    await waitFor(async () => !fs.existsSync(SOCK), 20000);
    check('server restarted', await startServer());
    const back = await waitFor(companionLive, 40000, 500);
    check('the same companion re-registers (lease live)', back, await sessions());
    const after = await waitFor(async () => {
      const s = await sessions();
      return s.length >= before.length && s.length ? s : null;
    }, 10000);
    check('browser sessions are restored from the snapshot', after, { before, after });
    const viewsAfter = (await views()).map((x) => x.view_id).sort();
    check('the companion kept its views (same process, same view ids)', JSON.stringify(viewsAfter) === JSON.stringify(viewsBefore), { viewsBefore, viewsAfter });
    const panes = await panesOf();
    p1 = (panes.find((p) => p.tab_id === t1) || {}).pane_id || p1;
    const o = await browser(['open', `${W}/e`], p1);
    check('an agent open after the restart lands in the companion again', o.result && o.result.data && o.result.data.host === 'companion', o.error || o.result.data);
  });

  await section('page dialogs across a server restart: a page sitting in a prompt never stalls the attach', async () => {
    // the page is blocked in its sync dialog call while herdr (and its sidecar) restart; its
    // renderer answers no CDP until then, which would stall the new sidecar's attach (seen:
    // connectOverCDP timed out twice, host_unavailable): the companion cancels it first
    await browser(['eval', `setTimeout(() => { window.__r = JSON.stringify(prompt('across a restart', 'k')); }, 1500); 'armed'`], p1);
    const before = await waitFor(async () => {
      const r = await browser(['read', '--format', 'text'], p1);
      return r.error && r.error.code === 'dialog_open' ? r.error : null;
    }, 8000, 300);
    check('the prompt is open before the restart', before, before);
    await apiOk('server.stop', {});
    await waitFor(async () => !fs.existsSync(SOCK), 20000);
    check('server restarted with the prompt open', await startServer());
    const t0 = Date.now();
    const back = await waitFor(companionLive, 40000, 500);
    check('the companion re-registers at once (no attach stall)', back && Date.now() - t0 < 15000, { ms: Date.now() - t0, sessions: await sessions() });
    const panes = await panesOf();
    p1 = (panes.find((p) => p.tab_id === t1) || {}).pane_id || p1;
    const got = await waitFor(async () => {
      const r = await browser(['eval', 'window.__r'], p1);
      return !r.error && /null/.test(text(r)) ? text(r) : null;
    }, 10000, 300);
    check('the prompt was cancelled (the page got null) and the agent works on its tab', got, got);
  });

  await section('live handoff: groups survive, the companion stays attached', async () => {
    const before = await sessions();
    const h = await apiOk('server.live_handoff', { import_exe: BIN }, 60000);
    check('server.live_handoff answers', !h.error, h.error);
    await sleep(2000);
    const back = await waitFor(() => companionLive({ newLease: false }), 40000, 500);
    check('the companion is attached after the handoff (lease live)', back, await sessions());
    const after = await sessions();
    check('browser sessions carried over', after.length === before.length && after.every((s) => before.some((b) => b.tab_id === s.tab_id && b.pages === s.pages)), { before, after });
    const r = await browser(['read', '--format', 'text'], p1);
    check('the agent keeps working on its tab after the handoff', !r.error && /E/.test(text(r)), r.error || text(r).slice(0, 100));
  });

  await section('a live handoff while a page sits in a prompt', async () => {
    await browser(['eval', `setTimeout(() => { window.__r = JSON.stringify(prompt('across a handoff', 'k')); }, 1500); 'armed'`], p1);
    const before = await waitFor(async () => {
      const r = await browser(['read', '--format', 'text'], p1);
      return r.error && r.error.code === 'dialog_open' ? r.error : null;
    }, 8000, 300);
    check('the prompt is open before the handoff', before, before);
    const t0 = Date.now();
    const h = await apiOk('server.live_handoff', { import_exe: BIN }, 60000);
    check('server.live_handoff answers with the prompt open', !h.error, h.error);
    await sleep(2000);
    const back = await waitFor(() => companionLive({ newLease: false }), 40000, 500);
    check('the companion is attached after it', back, await sessions());
    const r = await browser(['read', '--format', 'text'], p1, 90000);
    const seen = r.error && r.error.code === 'dialog_open' && /across a handoff/.test(r.error.message);
    check('the new sidecar sees the open prompt or the page is free (no stall)', (seen || !r.error) && Date.now() - t0 < 45000, { ms: Date.now() - t0, r: r.error || text(r).slice(0, 120) });
    if (seen) {
      const d = await browser(['dialog', 'accept', 'z'], p1);
      check('… and answers it', !d.error, d.error);
    }
    const free = await waitFor(async () => {
      const x = await browser(['eval', 'window.__r'], p1);
      return !x.error && /"z"|null/.test(text(x)) ? text(x) : null;
    }, 8000, 300);
    check('the page got its answer', free, free);
  });

  await section('page dialogs after a handoff: the new sidecar still sees and answers them', async () => {
    await browser(['eval', `setTimeout(() => { window.__r = JSON.stringify(prompt('after handoff', 'h')); }, 1500); 'armed'`], p1);
    const b = await waitFor(async () => {
      const r = await browser(['read', '--format', 'text'], p1);
      return r.error && r.error.code === 'dialog_open' ? r.error : null;
    }, 8000, 300);
    check('the prompt is seen after the handoff', b, b);
    const d = await browser(['dialog', 'accept', 'y'], p1);
    const got = await waitFor(async () => {
      const r = await browser(['eval', 'window.__r'], p1);
      return !r.error && /"y"/.test(text(r)) ? text(r) : null;
    }, 5000, 300);
    check('… and answered', !d.error && got, d.error || got);
  });

  await section('cross-workspace move: the moved tab keeps its live views', async () => {
    const w2 = await api('workspace.create', { label: 'e2e-two', focus: false });
    const ws2 = (w2.workspace && w2.workspace.workspace_id) || w2.workspace_id;
    const ws2Tabs = ((await api('tab.list', { workspace_id: ws2 })).tabs || []).map((t) => t.tab_id); // its default tab
    const pane2 = ((await panesOf()).find((p) => p.tab_id === t2) || {}).pane_id;
    const ctlOpen = (a) => within(insp.control().evaluate((x) => globalThis.__herdrCompanion.open(x), a), 10000, 'open()');
    const live = await ctlOpen({ tab_id: t2, url: `${W}/a`, background: false });
    check(`a live view in ${t2}`, live && live.view_id && !live.error, live);
    const before = (await views()).filter((x) => x.tab_id === t2).map((x) => x.view_id).sort();
    const unknownBefore = (fs.readFileSync(path.join(T, 'companion.log'), 'utf8').match(/sync named unknown tabs/g) || []).length;
    // the pane is the tab's only one: it moves as a whole tab (new tab id, same browser group)
    const mv = await apiOk('pane.move', { pane_id: pane2, destination: { type: 'new_tab', workspace_id: ws2 }, focus: false });
    // a sync right away still names the old tab id (herdr answers it in unknown_tabs)
    const raced = await within(insp.control().evaluate((x) => globalThis.__herdrCompanion.open(x), { tab_id: t2, url: `${W}/b` }), 10000, 'open()').catch((e) => ({ error: String(e) }));
    const t2new =
      (mv.move_result && mv.move_result.pane && mv.move_result.pane.tab_id) ||
      (await waitFor(async () => {
        const t = ((await api('tab.list', { workspace_id: ws2 })).tabs || []).find((x) => !ws2Tabs.includes(x.tab_id));
        return t && t.tab_id;
      }, 10000));
    check('pane.move moves the tab to the other workspace', !mv.error && t2new && t2new !== t2, mv.error || { t2new, raced });
    const moved = await waitFor(async () => {
      const v = (await views()).filter((x) => x.tab_id === t2new).map((x) => x.view_id);
      return before.every((id) => v.includes(id)) ? v : null;
    }, 15000);
    check(`the companion follows the group to ${t2new} with the same views`, moved, { before, now: await views() });
    check('the live view is still live', ((await views()).find((x) => x.view_id === live.view_id) || {}).live === true);
    const s = await waitFor(async () => (await sessions()).find((x) => x.tab_id === t2new && x.pages >= before.length), 10000);
    check(`herdr's browser sessions follow the move (${t2new})`, s, await sessions());
    const unknownAfter = (fs.readFileSync(path.join(T, 'companion.log'), 'utf8').match(/sync named unknown tabs/g) || []).length;
    console.log(`  (syncs that named the old tab id: ${unknownAfter - unknownBefore})`);
    t2 = t2new;
  });

  await section('split syncs: every group reaches herdr under a small sync budget', async () => {
    const all = await views();
    const tabs = [...new Set(all.map((x) => x.tab_id))];
    const s = await waitFor(async () => {
      const ss = await sessions();
      return tabs.every((t) => (ss.find((x) => x.tab_id === t) || {}).pages === all.filter((x) => x.tab_id === t).length) ? ss : null;
    }, 15000);
    check(`herdr has every group's pages (${tabs.length} groups, budget ${SYNC_BUDGET} bytes)`, s, { views: all.map((x) => [x.tab_id, x.view_id]), sessions: await sessions() });
    const log = fs.readFileSync(path.join(T, 'companion.log'), 'utf8');
    check('no sync was refused (too_large) or failed', !/host_sync failed/.test(log), (log.match(/host_sync failed.*/g) || []).slice(0, 3));
  });
} finally {
  await section('teardown', async () => {
    if (child) {
      if (!insp) await inspector().catch(() => {});
      check('companion quits', await quitCompanion());
    }
  });
  web.close();
  console.log(`\n${results.length - failed}/${results.length} passed; logs in ${T} (server.log, companion.log)`);
  process.exit(failed ? 1 : 0);
}

