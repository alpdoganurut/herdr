// Driver of test/e2e-placements.sh (run that, not this): the browser dock end to end.
//
// A real herdr server and TUI client (in a pseudo-terminal of 160x45 cells, 2568x1546 px:
// a Retina Ghostty's 16x34 px cells) and this checkout's companion (headless, test hooks,
// its window source a JSON file playing a Ghostty window owned by the client's terminal).
// Covered: prefix+B docks the browser and the client publishes its placement; the companion
// binds the terminal window, covers the band where the geometry model puts it and reports
// `covering` (herdr's browser.get shows it); a window that no longer fits the grid is
// `unsupported` with the reason; off screen is `hidden`; Browser view (prefix+Z) is a `full`
// placement over the pane area; undocking clears it and the companion leaves docked mode;
// docking again is reported anew.
import { spawn, execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');
const { Connection } = require('../src/herdr-api.js');
const { placementRect } = require('../src/dock/geometry.js');

const T = process.env.HERDR_E2E_TMP;
const BIN = process.env.HERDR_E2E_BIN;
const SOCKETS = (process.env.HERDR_E2E_SOCKETS || '').split(':').filter(Boolean);
const PY = process.env.HERDR_E2E_PYTHON || 'python3';
let SOCK = null; // the one the server opens (herdr/ for a release build, herdr-dev/ for debug)
if (!T || !BIN || !SOCKETS.length) {
  console.error('run test/e2e-placements.sh');
  process.exit(2);
}
const APP = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const ELECTRON = require('electron');
const WINDOWS = path.join(T, 'windows.json');
const COMMANDS = path.join(T, 'client.commands');
const GRID = { cols: 160, rows: 45, xpx: 160 * 16 + 8, ypx: 45 * 34 + 16 };
// The fake Ghostty window: the surface (extent + 2 pt padding each side) and a 28 pt titlebar.
const FRAME = { x: 120, y: 90, w: (GRID.xpx + 8) / 2, h: (GRID.ypx + 8) / 2 + 28 };

let failed = 0;
const check = (name, ok, detail) => {
  if (!ok) failed++;
  const d = detail === undefined ? '' : ' — ' + (typeof detail === 'string' ? detail : JSON.stringify(detail)).slice(0, 600);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${d}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, ms = 15000, step = 150) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {}
    await sleep(step);
  }
  return null;
}
const remember = (pid) => fs.appendFileSync(path.join(T, 'pids'), `${pid}\n`);

async function api(method, params = {}, timeoutMs = 20000) {
  const c = new Connection(SOCK, { prefix: 'e2e', timeoutMs });
  c.on('error', () => {});
  await c.connect();
  try {
    return await c.request(method, params, { timeoutMs });
  } finally {
    c.close();
  }
}
const statusOf = async (id) => {
  const info = (await api('browser.get', {})).browser || {};
  return (info.placements || []).find((p) => p.id === id) || null;
};

const writeWindows = (windows) => fs.writeFileSync(WINDOWS + '.tmp', JSON.stringify({ windows })) || fs.renameSync(WINDOWS + '.tmp', WINDOWS);
const type = (hex) => fs.appendFileSync(COMMANDS, `${hex}\n`);

setTimeout(() => {
  console.log('FAIL the run did not finish within 6 min');
  process.exit(1);
}, 6 * 60 * 1000).unref();
// the wrapper's cleanup quits what this run started (the companion through its control page)
process.on('unhandledRejection', (e) => {
  console.log(`FAIL unexpected: ${e && e.stack ? e.stack : e}`);
  process.exit(1);
});

// ---------- herdr ----------
const serverLog = fs.openSync(path.join(T, 'server.log'), 'a');
const server = spawn(BIN, ['server'], { stdio: ['ignore', serverLog, serverLog], env: process.env });
remember(server.pid);
check('the isolated server comes up', await waitFor(() => {
  SOCK = SOCKETS.find((p) => fs.existsSync(p)) || null;
  return SOCK && api('browser.get', {}).then(() => true);
}, 30000));
if (!SOCK) process.exit(1);

// ---------- the companion (this checkout, unmanaged, headless) ----------
writeWindows([]);
const companionLog = fs.openSync(path.join(T, 'companion.log'), 'a');
const companion = spawn(ELECTRON, [APP, '--socket', SOCK, '--headless', '--profile-dir', path.join(T, 'profile')], {
  stdio: ['ignore', companionLog, companionLog],
  env: { ...process.env, HERDR_COMPANION_TEST_HOOKS: '1', HERDR_COMPANION_FAKE_WINDOWS: WINDOWS, HERDR_COMPANION_FAKE_SCALE: '2' },
});
remember(companion.pid);
const port = await waitFor(async () => {
  const info = (await api('browser.get', {})).browser || {};
  const p = (info.profiles || []).find((x) => x.name === 'companion' && x.state === 'running');
  return p && p.port;
}, 60000);
check('the companion registers', !!port);
const cdp = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 20000 });
const control = cdp.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith('herdr-companion://control/'));
const debug = () => control.evaluate(() => globalThis.__herdrCompanion.dock_debug());

// ---------- the TUI client in a pty ----------
fs.writeFileSync(COMMANDS, '');
const clientLog = fs.openSync(path.join(T, 'client.log'), 'a');
const client = spawn(PY, ['-I', path.join(APP, 'test', 'pty-client.py'), String(GRID.cols), String(GRID.rows), String(GRID.xpx), String(GRID.ypx), COMMANDS, BIN], {
  stdio: ['ignore', clientLog, clientLog],
  env: process.env,
});
remember(client.pid);
// The client's terminal: the pty driver (the client's parent) plays the Ghostty process.
const attached = await waitFor(async () => {
  const out = execFileSync('ps', ['-o', 'pid=,ppid=', '-A'], { encoding: 'utf8' });
  return out.split('\n').map((l) => l.trim().split(/\s+/).map(Number)).find(([, ppid]) => ppid === client.pid);
}, 15000);
check('the TUI client runs in its pty', !!attached);
const terminalPid = client.pid;
const ghostty = { num: 4101, pid: terminalPid, x: FRAME.x, y: FRAME.y, w: FRAME.w, h: FRAME.h, on: true };
writeWindows([ghostty]);
await sleep(2500); // the client's first frames and snapshot

// ---------- dock: prefix+B ----------
type('02 42');
const covering = await waitFor(async () => {
  const d = await debug();
  return d && d.decision && d.decision.reports && d.decision.reports.find((r) => r.state === 'covering') ? d : null;
}, 20000);
check('prefix+B docks: the companion covers the placement', !!covering, covering ? undefined : await debug().catch((e) => e.message));
if (covering) {
  const target = covering.decision.target.placement;
  check('the placement is the band at the right edge', target.kind === 'side' && target.area.x + target.area.width === GRID.cols && target.grid.cols === GRID.cols, target.area);
  check('the placement carries the terminal pixels and the client ancestry', target.extent_px.width === GRID.xpx && target.cell_px.width === 16 && target.cell_px.height === 34 && target.pids.includes(terminalPid), { px: target.extent_px, pids: target.pids });
  check('bound to the terminal window', covering.bound && covering.bound.wid === ghostty.num, covering.bound);
  const expected = placementRect({ placement: target, frame: FRAME, scale: 2, config: null });
  check('the dock window sits where the model puts it', expected.ok && JSON.stringify(covering.bounds) === JSON.stringify(expected.rect), { bounds: covering.bounds, expected: expected.rect });
  check('the band runs to the window edges', covering.bounds.x + covering.bounds.width === FRAME.x + FRAME.w && covering.bounds.y + covering.bounds.height === FRAME.y + FRAME.h, covering.bounds);
  const reported = await waitFor(async () => {
    const s = await statusOf(target.id);
    return s && s.state === 'covering' ? s : null;
  });
  check("herdr's browser.get shows the report", !!reported);

  // a window that no longer matches the grid (the client's surface is a split): unsupported
  writeWindows([{ ...ghostty, w: FRAME.w * 2 }]);
  const unsupported = await waitFor(async () => {
    const s = await statusOf(target.id);
    return s && s.state === 'unsupported' ? s : null;
  }, 15000);
  check('a window that does not fit the grid is unsupported, with the reason', !!unsupported && /Ghostty/.test(unsupported.reason || ''), unsupported);
  writeWindows([ghostty]);
  check('back once it fits', !!(await waitFor(async () => ((await statusOf(target.id)) || {}).state === 'covering')));

  // off screen (minimised, hidden, another Space): hidden
  writeWindows([{ ...ghostty, on: false }]);
  const hidden = await waitFor(async () => ((await statusOf(target.id)) || {}).state === 'hidden');
  check('a window off screen hides the dock', !!hidden);
  writeWindows([ghostty]);
  await waitFor(async () => ((await statusOf(target.id)) || {}).state === 'covering');

  // ---------- a tab switch without input (an agent's or the companion's tab.focus) ----------
  // The client re-publishes its placement with the snapshot that moves its tab (no key, no
  // mouse: the pty types nothing), well inside the 2 s browser refresh that would otherwise
  // carry it.
  const docked = target.tab_id;
  const created = await api('tab.create', { label: 'plain', focus: false });
  const plain = (created.tab && created.tab.tab_id) || created.tab_id;
  await sleep(300);
  let started = Date.now();
  await api('tab.focus', { tab_id: plain });
  const away = await waitFor(async () => {
    const d = await debug();
    const p = d && d.decision && d.decision.target;
    const s = await statusOf(target.id);
    return !p && d && !d.shown && s && s.state === 'hidden' ? { ms: Date.now() - started } : null;
  }, 5000, 50);
  check('a tab.focus to a tab without the dock unshows the placement at once', !!away && away.ms < 1200, away || (await debug().catch((e) => e.message)));
  started = Date.now();
  await api('tab.focus', { tab_id: docked });
  const back = await waitFor(async () => {
    const d = await debug();
    return d && d.shown && d.decision.target && d.decision.target.placement.tab_id === docked ? { ms: Date.now() - started } : null;
  }, 5000, 50);
  check('and back on the docked tab the dock covers it again at once', !!back && back.ms < 1200, back || (await debug().catch((e) => e.message)));

  // ---------- Browser view: prefix+Z ----------
  type('02 5a');
  const full = await waitFor(async () => {
    const d = await debug();
    const p = d && d.decision && d.decision.target && d.decision.target.placement;
    return p && p.kind === 'full' && d.decision.reports.find((r) => r.state === 'covering') ? d : null;
  }, 15000);
  check('prefix+Z: Browser view is a full placement over the pane area', !!full, full ? undefined : await debug().catch((e) => e.message));
  if (full) {
    const p = full.decision.target.placement;
    check('the full area runs from the pane surface to the right edge', p.area.x > 0 && p.area.x + p.area.width === GRID.cols, p.area);
    const want = placementRect({ placement: p, frame: FRAME, scale: 2, config: null });
    check('the dock window covers it', JSON.stringify(full.bounds) === JSON.stringify(want.rect), { bounds: full.bounds, want: want.rect });
  }
  type('02 5a');
  await waitFor(async () => {
    const d = await debug();
    return d.decision.target && d.decision.target.placement.kind === 'side';
  });

  // ---------- undock ----------
  type('02 42');
  const undocked = await waitFor(async () => {
    const d = await debug();
    return d && !d.docked ? d : null;
  }, 15000);
  check('prefix+B again: the placement is cleared and the companion leaves docked mode', !!undocked);
  check('the report goes with the placement', !(await statusOf(target.id)));

  // ---------- docked again: the same placement id, reported anew ----------
  type('02 42');
  const redocked = await waitFor(async () => ((await statusOf(target.id)) || {}).state === 'covering', 20000);
  check('docked again: the companion reports on the placement anew', !!redocked, redocked ? undefined : await debug().catch((e) => e.message));
  type('02 42');
  await waitFor(async () => {
    const d = await debug();
    return d && !d.docked;
  }, 15000);
}

// ---------- done ----------
type('quit');
await control.evaluate(() => globalThis.__herdrCompanion.quit()).catch(() => {});
await cdp.close().catch(() => {});
await waitFor(() => companion.exitCode !== null, 10000);
check('the companion quits through its control page', companion.exitCode !== null);
await waitFor(() => client.exitCode !== null, 10000);
console.log(failed ? `\n${failed} check(s) FAILED` : '\nall checks passed');
process.exit(failed ? 1 : 0);
