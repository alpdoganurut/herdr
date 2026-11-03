// Driver of test/e2e-launcher.sh (run that, not this): herdr's companion launcher against a real,
// isolated herdr. Expects HERDR_E2E_TMP / HERDR_E2E_BIN / HERDR_E2E_SOCKET and an isolated
// environment with HERDR_COMPANION_ELECTRON_DIST, HERDR_COMPANION_HEADLESS=1 and
// HERDR_COMPANION_TEST_HOOKS=1.
//
// Covered: setup --companion (install, the no-op rerun, the doctor), an agent's open starting the
// companion in the background, crash restarts with backoff (1, 2, 4 s scheduled), the give-up after
// 5 crashes in 5 minutes and the error an open then gets, restart clearing it, a clean stop (no
// signal needed), a crash and a restart keeping the agent's tab id and current tab, survival of a live handoff (same pid, re-registered, adopted), the stop with the
// server.
import { spawn, execFile, execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');
const { Connection } = require('../src/herdr-api.js');

const T = process.env.HERDR_E2E_TMP;
const BIN = process.env.HERDR_E2E_BIN;
const SOCK = process.env.HERDR_E2E_SOCKET;
if (!T || !BIN || !SOCK || !process.env.HERDR_COMPANION_ELECTRON_DIST) {
  console.error('run test/e2e-launcher.sh');
  process.exit(2);
}
const HOME_DIR = path.join(T, 'state', 'herdr', 'browser', 'companion-app');
const RUN_JSON = path.join(HOME_DIR, 'run.json');

const results = [];
let failed = 0;
const check = (name, ok, detail) => {
  results.push({ name, ok: !!ok });
  if (!ok) failed++;
  const d = detail === undefined ? '' : ' — ' + (typeof detail === 'string' ? detail : JSON.stringify(detail)).slice(0, 500);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${d}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const within = (p, ms, what) => {
  let timer;
  return Promise.race([
    Promise.resolve(p).finally(() => clearTimeout(timer)),
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${what} timed out after ${ms} ms`)), ms);
    }),
  ]);
};
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
const SECTION_MS = 4 * 60 * 1000;
async function section(name, fn) {
  console.log(`\n== ${name}`);
  try {
    await within(fn(), SECTION_MS, `section "${name}"`);
  } catch (e) {
    check(`${name}: no exception`, false, e && e.stack ? e.stack : String(e));
  }
}
const alive = (pid) => {
  if (!pid) return false;
  try {
    process.kill(pid, 0); // signal 0: a probe, never a signal
    return true;
  } catch (e) {
    return e.code === 'EPERM';
  }
};
const argsOf = (pid) => {
  try {
    return execFileSync('ps', ['-o', 'args=', '-p', String(pid)], { encoding: 'utf8' });
  } catch {
    return '';
  }
};
const runRecord = () => {
  try {
    return JSON.parse(fs.readFileSync(RUN_JSON, 'utf8'));
  } catch {
    return null;
  }
};

// ---------- herdr ----------

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
const companion = async () => ((await apiOk('browser.companion', { action: 'status' })).companion) || {};

function cli(args, timeoutMs = 120000) {
  return new Promise((resolve) => {
    execFile(BIN, args, { timeout: timeoutMs, maxBuffer: 16 << 20, env: process.env }, (err, stdout, stderr) => {
      resolve({ code: err ? err.code || 1 : 0, stdout: String(stdout), stderr: String(stderr) });
    });
  });
}
// `herdr browser <args> --json` as an agent in the session's pane: {result} or {error}.
let PANE = null;
async function browser(args, timeoutMs = 90000) {
  const r = await cli(['browser', ...args, '--json', ...(PANE ? ['--pane', PANE] : [])], timeoutMs);
  for (const line of `${r.stdout}\n${r.stderr}`.split('\n')) {
    try {
      const x = JSON.parse(line);
      if (x && x.error) return { error: x.error };
      if (x && x.result) return { result: x.result.result || x.result };
    } catch {}
  }
  return { error: { code: 'cli', message: `${r.stderr} ${r.stdout}`.trim().slice(0, 400) } };
}

let serverPids = [];
async function startServer() {
  const out = fs.openSync(path.join(T, 'server.log'), 'a');
  const p = spawn(BIN, ['server'], { stdio: ['ignore', out, out], env: process.env });
  serverPids.push(p.pid);
  fs.appendFileSync(path.join(T, 'server.pids'), `${p.pid}\n`);
  const up = await waitFor(() => fs.existsSync(SOCK) && api('browser.get', {}).then(() => true), 20000);
  return !!up;
}

// The companion's control page over its DevTools port (the e2e plays a user with test hooks).
async function control() {
  const port = await waitFor(async () => {
    const info = (await api('browser.get', {})).browser || {};
    const p = (info.profiles || []).find((x) => x.name === 'companion' && x.state === 'running');
    return p && p.port;
  }, 20000);
  if (!port) throw new Error('no companion port');
  const b = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 20000 });
  const page = b.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith('herdr-companion://control/'));
  return { b, call: (fn) => within(page.evaluate((f) => globalThis.__herdrCompanion[f](), fn), 10000, fn) };
}
async function crash() {
  const c = await control();
  await c.call('__crash').catch(() => {}); // the process exits under the call
  await within(c.b.close(), 5000, 'inspector close').catch(() => {});
}

// ---------- a local web server ----------

const web = http.createServer((req, res) => {
  res.setHeader('content-type', 'text/html');
  res.end(`<title>page ${req.url}</title><h1>${req.url}</h1>`);
});
await new Promise((r) => web.listen(0, '127.0.0.1', r));
const W = `http://127.0.0.1:${web.address().port}`;

setTimeout(() => {
  console.log('FAIL the run did not finish within 20 min');
  process.exit(1);
}, 20 * 60 * 1000).unref();

try {
  await section('setup --companion: install once, a rerun writes nothing', async () => {
    const first = await cli(['browser', 'setup', '--companion', '--yes'], 300000);
    check('setup --companion succeeds', first.code === 0, first.stdout + first.stderr);
    check('it reports the install', /companion: app \d+\.\d+\.\d+ \([0-9a-f]{12}\) · electron 45\.2\.1/.test(first.stdout), first.stdout);
    const install = JSON.parse(fs.readFileSync(path.join(HOME_DIR, 'install.json'), 'utf8'));
    check('install.json stamps the app and Electron', install.app && install.electron === '45.2.1' && install.app_sha256.length === 64, install);
    if (process.platform === 'darwin') {
      check('the bundle is "herdr+ companion.app"', /herdr\+ companion\.app$/.test(install.bundle) && install.branded, install);
    }
    const stamp = fs.statSync(path.join(HOME_DIR, 'install.json')).mtimeMs;
    const again = await cli(['browser', 'setup', '--companion', '--yes'], 120000);
    check('a rerun is up to date and writes nothing', /companion: up to date/.test(again.stdout) && fs.statSync(path.join(HOME_DIR, 'install.json')).mtimeMs === stamp, again.stdout);
  });

  check('herdr server started', await startServer());

  await section('a session with a pane for the agent', async () => {
    const w = await api('workspace.create', { label: 'e2e', focus: true });
    const ws = (w.workspace && w.workspace.workspace_id) || w.workspace_id;
    const panes = (await api('pane.list', { workspace_id: ws })).panes || [];
    PANE = panes[0] && panes[0].pane_id;
    check('a workspace and its pane', ws && PANE, { ws, panes });
  });

  await section('doctor and status before the first open', async () => {
    const d = await cli(['browser', 'doctor', '--json']);
    let checks = [];
    try {
      checks = JSON.parse(d.stdout);
    } catch {}
    const c = checks.find((x) => x.id === 'companion_app');
    check('doctor has a companion_app check that is ok', c && c.ok, c || d.stdout);
    const s = await companion();
    check('the companion is stopped (installed, never started)', s.state === 'stopped', s);
  });

  let pid1;
  await section('an agent open starts it in the background and waits for it', async () => {
    const t0 = Date.now();
    const o = await browser(['open', `${W}/a`]);
    const ms = Date.now() - t0;
    check('the open lands in the companion', o.result && o.result.data && o.result.data.host === 'companion', o.error || o.result);
    check(`started and registered within 10 s (${ms} ms)`, ms < 10000);
    const s = await companion();
    pid1 = s.pid;
    check('running, managed, with a pid', s.state === 'running' && s.managed && pid1, s);
    check('launched with --background --managed (never shown)', /--background/.test(argsOf(pid1)) && /--managed/.test(argsOf(pid1)), argsOf(pid1));
    const run = runRecord();
    check('run.json names the process and this server socket', run && run.pid === pid1 && run.socket === SOCK, run);
    const st = await cli(['browser', 'companion', 'status']);
    check('`browser companion status` reads it', /^companion running · pid \d+ · up \d+s · 0 restarts · v/.test(st.stdout), st.stdout);
  });

  await section('crash restarts back off 1, 2, 4 s; the fifth crash gives up', async () => {
    const gaps = [];
    for (let n = 1; n <= 4; n++) {
      const before = await companion();
      await crash();
      const crashed = await waitFor(async () => {
        const s = await companion();
        return s.state === 'crashed' ? s : null;
      }, 10000, 50);
      check(`crash ${n}: crashed with a restart scheduled`, crashed && crashed.next_retry_unix, crashed);
      if (crashed) gaps.push(crashed.next_retry_unix - crashed.since_unix);
      const back = await waitFor(async () => {
        const s = await companion();
        return s.state === 'running' && s.pid && s.pid !== before.pid ? s : null;
      }, 30000, 100);
      check(`crash ${n}: running again as a new process`, back, await companion());
      if (back) check(`crash ${n}: the restart is in the background`, /--background/.test(argsOf(back.pid)));
    }
    check('backoff steps 1, 2, 4, 8 s (scheduled)', JSON.stringify(gaps) === JSON.stringify([1, 2, 4, 8]), gaps);
    await crash();
    const failedState = await waitFor(async () => {
      const s = await companion();
      return s.state === 'failed' ? s : null;
    }, 10000, 100);
    check('the fifth crash in five minutes: failed', failedState && failedState.restarts === 5 && failedState.last_error, failedState || (await companion()));
    const o = await browser(['open', `${W}/b`]);
    check(
      'an open now names the give-up and the restart command',
      o.error && o.error.code === 'host_unavailable' && /crashed 5 times in 5 minutes and herdr stopped restarting it \(last error: .*\); ask the user to run "herdr browser companion restart"/.test(o.error.message),
      o.error || o.result,
    );
  });

  await section('restart clears the give-up', async () => {
    const r = await cli(['browser', 'companion', 'restart']);
    check('`browser companion restart` answers', r.code === 0 && /companion restarting/.test(r.stdout), r.stdout + r.stderr);
    const s = await waitFor(async () => {
      const x = await companion();
      return x.state === 'running' ? x : null;
    }, 30000, 200);
    check('running again, restarts back to 0', s && !s.restarts && !s.last_error, s || (await companion()));
    check('a user restart from a plain terminal shows its window (no --background)', s && !/--background/.test(argsOf(s.pid)), s && argsOf(s.pid));
  });

  // GAP 1: a restarted companion restores the agent's page under a new target; herdr keeps its
  // herdr tab id, its opener and the pane's current tab, so the agent's next call just works.
  await section("a crash and a restart keep the agent's tab id and current tab", async () => {
    const o = await browser(['open', `${W}/r`]);
    check('an agent opens a page', o.result && o.result.data && o.result.data.host === 'companion', o.error || o.result);
    const ledger = async () => ((await api('browser.get', {})).browser || {}).tabs || [];
    const mine = async () => (await ledger()).find((t) => t.profile === 'companion' && /\/r$/.test(t.url) && t.state === 'open');
    const first = await waitFor(mine, 10000);
    check('the ledger has it as the agent\'s', first && JSON.stringify(first.opened_by).includes(PANE), first);
    if (!first) return;
    for (const how of ['crash', 'restart']) {
      const before = await companion();
      if (how === 'crash') await crash();
      else {
        const r = await cli(['browser', 'companion', 'restart']);
        check('`browser companion restart` answers', r.code === 0, r.stdout + r.stderr);
      }
      const back = await waitFor(async () => {
        const s = await companion();
        return s.state === 'running' && s.pid && s.pid !== before.pid ? s : null;
      }, 30000, 100);
      check(`${how}: running again as a new process`, back, await companion());
      const again = await waitFor(async () => {
        const t = (await ledger()).find((x) => x.id === first.id);
        return t && t.state === 'open' && t.target_id !== first.target_id ? t : null;
      }, 15000, 200);
      check(`${how}: the page is open again under its herdr tab id ${first.id} (a new target)`, again, (await ledger()).filter((t) => t.profile === 'companion'));
      check(`${how}: still opened by the agent's pane`, again && JSON.stringify(again.opened_by).includes(PANE) && !JSON.stringify(again.last_actor).includes('"user"'), again);
      const dupes = (await ledger()).filter((t) => t.profile === 'companion' && t.state === 'open' && /\/r$/.test(t.url));
      check(`${how}: no second record for the page`, dupes.length === 1, dupes);
      const read = await browser(['read', '--format', 'text']);
      check(`${how}: the agent's next call (no tab given) reads its page`, !read.error && /\/r/.test(JSON.stringify(read.result)), read.error || read.result);
      const old = await browser(['read', first.id, '--format', 'text']);
      check(`${how}: the old id still answers`, !old.error, old.error);
    }
  });

  await section('stop quits it cleanly (control page, no signal needed)', async () => {
    const pid = (await companion()).pid;
    const t0 = Date.now();
    const r = await cli(['browser', 'companion', 'stop']);
    check('`browser companion stop` answers', r.code === 0, r.stdout + r.stderr);
    const gone = await waitFor(async () => !alive(pid), 10000, 100);
    const ms = Date.now() - t0;
    check(`the process exited (${ms} ms)`, gone);
    check('quickly: the control page quit, before the SIGTERM step (< 3 s)', ms < 3000, ms);
    const s = await waitFor(async () => {
      const x = await companion();
      return x.state === 'stopped' ? x : null;
    }, 10000);
    const cleared = await waitFor(async () => !runRecord(), 5000, 100);
    check('stopped, not a crash; run.json removed', s && !s.restarts && cleared, { s, run: runRecord() });
    const log = fs.readFileSync(path.join(HOME_DIR, 'companion.log'), 'utf8');
    check('it never needed SIGTERM', !/SIGTERM: quitting/.test(log.split('\n').slice(-40).join('\n')));
  });

  await section('a live handoff keeps the same process', async () => {
    const o = await browser(['open', `${W}/c`]);
    check('an open starts it again', o.result && o.result.data && o.result.data.host === 'companion', o.error);
    const tab = o.result && (o.result.tab || (o.result.data && o.result.data.tab));
    const before = await companion();
    const h = await apiOk('server.live_handoff', { import_exe: BIN }, 60000);
    check('server.live_handoff answers', !h.error, h.error);
    await sleep(1500);
    const after = await waitFor(async () => {
      const s = await companion();
      return s.state === 'running' && s.managed ? s : null;
    }, 30000, 250);
    check('the new server adopted it: same pid, managed, running', after && after.pid === before.pid, { before, after: after || (await companion()) });
    check('run.json still names it', (runRecord() || {}).pid === before.pid, runRecord());
    const r = await browser(['read', tab || 'companion:t1', '--format', 'text']);
    check('agents keep working on its pages', !r.error && /\/c/.test(JSON.stringify(r.result)), r.error || r.result);
  });

  await section('the companion goes with the server', async () => {
    const pid = (await companion()).pid;
    await apiOk('server.stop', {});
    const gone = await waitFor(async () => !alive(pid), 15000, 200);
    check('the companion exited with the server (within 8 s of the stop)', gone, { pid, args: argsOf(pid) });
    check('run.json removed', await waitFor(async () => !runRecord(), 5000, 100), runRecord());
  });
} finally {
  web.close();
  console.log(`\n${results.length - failed}/${results.length} passed; logs in ${T} (server.log, state/herdr/browser/companion-app/companion.log)`);
  process.exit(failed ? 1 : 0);
}
