// Driver of scripts/smoke_companion_dock.sh (run that, not this). See its header.
import { spawn, execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';

const T = process.env.HERDR_SMOKE_TMP;
const BIN = process.env.HERDR_SMOKE_BIN;
const APP = process.env.HERDR_SMOKE_APP;
const REPORT = process.env.HERDR_SMOKE_REPORT;
const WANT = (process.env.HERDR_SMOKE_VARIANTS || 'defaults').split(',').map((v) => v.trim()).filter(Boolean);
if (!T || !BIN || !APP) {
  console.error('run scripts/smoke_companion_dock.sh');
  process.exit(2);
}
const require = createRequire(path.join(APP, 'package.json'));
const { chromium } = require('playwright-core');
const { placementRect } = require(path.join(APP, 'src', 'dock', 'geometry.js'));
const { Connection } = require(path.join(APP, 'src', 'herdr-api.js'));
const GHOSTTY = '/Applications/Ghostty.app';
const GHOSTTY_EXE = '/Applications/Ghostty.app/Contents/MacOS/ghostty';

// Ghostty flags (points / cells) and the geometry config the model reads for them. `small`
// and `large` are the default config at other window sizes (100 columns: the narrowest
// window that still fits a band beside a 30-column terminal and the sidebar).
const VARIANTS = {
  small: { args: [], cols: 100, rows: 30, config: {} },
  defaults: { args: [], cols: 120, rows: 36, config: {} },
  large: { args: [], cols: 180, rows: 50, config: {} },
  padding: { args: ['--window-padding-x=10', '--window-padding-y=6'], cols: 110, rows: 32, config: { paddingX: [10, 10], paddingY: [6, 6] } },
  balanced: { args: ['--window-padding-balance=true'], cols: 104, rows: 30, config: { balance: true } },
  native: { args: ['--macos-titlebar-style=native'], cols: 132, rows: 38, config: { titlebarStyle: 'native' } },
  hidden: { args: ['--macos-titlebar-style=hidden'], cols: 140, rows: 40, config: { titlebarStyle: 'hidden' } },
};

const env = {
  ...Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('HERDR_'))),
  HOME: path.join(T, 'home'),
  XDG_CONFIG_HOME: path.join(T, 'config'),
  XDG_STATE_HOME: path.join(T, 'state'),
  XDG_DATA_HOME: path.join(T, 'data'),
  XDG_CACHE_HOME: path.join(T, 'cache'),
  HERDR_COMPANION_TEST_HOOKS: '1',
  HERDR_COMPANION_ELECTRON_DIST: path.join(APP, 'node_modules', 'electron', 'dist'),
};
for (const d of ['home', 'config/herdr', 'config/herdr-dev', 'state', 'data', 'cache']) fs.mkdirSync(path.join(T, d), { recursive: true });
const config = `onboarding = false\n\n[browser]\nenabled = true\nwindow = false\nhost = "companion"\nnode = "${process.execPath}"\n\n[ui]\nsidebar_layout = "tabs"\n`;
for (const d of ['herdr', 'herdr-dev']) fs.writeFileSync(path.join(T, 'config', d, 'config.toml'), config);
const SOCKETS = ['herdr', 'herdr-dev'].map((d) => path.join(T, 'config', d, 'herdr.sock'));
// The sidecar runtime herdr drives the companion through: this app's node_modules
// (playwright-core at the pin), as the e2e tests do.
for (const d of ['herdr', 'herdr-dev']) {
  const host = path.join(T, 'state', d, 'browser', 'host');
  fs.mkdirSync(host, { recursive: true });
  if (!fs.existsSync(path.join(host, 'node_modules'))) fs.symlinkSync(path.join(APP, 'node_modules'), path.join(host, 'node_modules'));
  fs.writeFileSync(path.join(host, 'runtime.json'), JSON.stringify({ version: 1, node: process.execPath, playwright_core: '1.63.0', assets_sha256: '', installed_at: 0 }) + '\n');
}

const lines = [];
const say = (s) => {
  console.log(s);
  lines.push(s);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, ms, step = 200) {
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
const sock = () => SOCKETS.find((p) => fs.existsSync(p));
async function api(method, params = {}, timeoutMs = 15000) {
  const c = new Connection(sock(), { prefix: 'smoke', timeoutMs });
  c.on('error', () => {});
  await c.connect();
  try {
    return await c.request(method, params, { timeoutMs });
  } finally {
    c.close();
  }
}
const jxa = (src, args = []) => execFileSync('/usr/bin/osascript', ['-l', 'JavaScript', '-e', src, ...args.map(String)], { encoding: 'utf8', timeout: 10000 }).trim();
// The measurement's window reader: a tiny Swift program over CGWindowList (no permission),
// compiled into the temp dir, independent of the companion's own probe (JXA through
// osascript). Without swiftc the JXA readers below measure instead (the report says so).
const CGWIN_SWIFT = `import CoreGraphics
import Foundation
func windows(_ options: CGWindowListOption, _ relativeTo: CGWindowID) -> [[String: Any]] {
    (CGWindowListCopyWindowInfo(options, relativeTo) as? [[String: Any]]) ?? []
}
func number(_ d: [String: Any], _ key: CFString) -> Int? { (d[key as String] as? NSNumber)?.intValue }
let args = Array(CommandLine.arguments.dropFirst())
var out: Any = []
if args.first == "list" {
    let pids = Set(args.dropFirst().compactMap { Int($0) })
    var list: [[String: Any]] = []
    for d in windows([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) {
        guard let pid = number(d, kCGWindowOwnerPID), pids.contains(pid), number(d, kCGWindowLayer) == 0,
              let num = number(d, kCGWindowNumber), let bounds = d[kCGWindowBounds as String] as? NSDictionary,
              let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { continue }
        list.append(["num": num, "pid": pid, "x": rect.origin.x, "y": rect.origin.y, "w": rect.size.width, "h": rect.size.height])
    }
    out = list
} else if args.first == "above", args.count > 1, let wid = UInt32(args[1]) {
    out = windows([.optionOnScreenAboveWindow], CGWindowID(wid)).filter { number($0, kCGWindowLayer) == 0 }.compactMap { number($0, kCGWindowNumber) }
} else {
    FileHandle.standardError.write(Data("usage: cgwin list <pid>... | cgwin above <wid>\\n".utf8))
    exit(2)
}
FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: out))
`;
const CGWIN = path.join(T, 'cgwin');
let MEASURE = 'jxa';
try {
  fs.writeFileSync(`${CGWIN}.swift`, CGWIN_SWIFT);
  execFileSync('/usr/bin/swiftc', ['-O', '-o', CGWIN, `${CGWIN}.swift`], { stdio: 'ignore', timeout: 180000 });
  MEASURE = 'swift';
} catch {}
const cgwin = (args) => JSON.parse(execFileSync(CGWIN, args.map(String), { encoding: 'utf8', timeout: 10000 }));
// On-screen layer-0 windows of `pids`, front to back (CGWindowList: no permission).
function windowsOf(pids) {
  if (MEASURE === 'swift') return cgwin(['list', ...pids]);
  const src = `ObjC.import('CoreGraphics');
function run(argv) { const pids = argv.map(Number); const a = ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0)); const o = [];
for (let i = 0; i < a.count; i++) { const d = a.objectAtIndex(i); const pid = d.objectForKey('kCGWindowOwnerPID').js; if (!pids.includes(pid) || d.objectForKey('kCGWindowLayer').js !== 0) continue;
const b = d.objectForKey('kCGWindowBounds'); o.push({ num: d.objectForKey('kCGWindowNumber').js, pid, x: b.objectForKey('X').js, y: b.objectForKey('Y').js, w: b.objectForKey('Width').js, h: b.objectForKey('Height').js }); }
return JSON.stringify(o); }`;
  return JSON.parse(jxa(src, pids));
}
function aboveOf(wid) {
  if (MEASURE === 'swift') return cgwin(['above', wid]);
  const src = `ObjC.import('CoreGraphics');
function run(argv) { const a = ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenAboveWindow, Number(argv[0]))); const o = [];
for (let i = 0; i < a.count; i++) { const d = a.objectAtIndex(i); if (d.objectForKey('kCGWindowLayer').js === 0) o.push(d.objectForKey('kCGWindowNumber').js); } return JSON.stringify(o); }`;
  return JSON.parse(jxa(src, [wid]));
}
function activate(pid) {
  const src = `ObjC.import('AppKit'); function run(argv) { const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
if (app.isNil()) return 'gone'; if (app.executableURL.path.js !== '${GHOSTTY_EXE}') return 'not ghostty'; return String(app.activateWithOptions($.NSApplicationActivateIgnoringOtherApps)); }`;
  return jxa(src, [pid]);
}
const ghosttyPids = () => {
  try {
    return execFileSync('/usr/bin/pgrep', ['-f', GHOSTTY_EXE], { encoding: 'utf8' }).split('\n').filter(Boolean).map(Number);
  } catch {
    return [];
  }
};
const alive = (pid) => {
  try {
    process.kill(pid, 0); // signal 0: a probe, never a signal
    return true;
  } catch (e) {
    return e.code === 'EPERM';
  }
};
const cpuSeconds = (pid) => {
  const t = execFileSync('/bin/ps', ['-o', 'time=', '-p', String(pid)], { encoding: 'utf8' }).trim();
  const parts = t.split(':').map(Number); // [h:]m:s.cc
  return parts.reduce((acc, v) => acc * 60 + v, 0);
};

say(`# Dock alignment smoke — ${new Date().toISOString()}`);
say('');
say(`herdr ${BIN}; Ghostty ${execFileSync(GHOSTTY_EXE, ['--version'], { encoding: 'utf8' }).split('\n')[0]}; macOS ${execFileSync('/usr/bin/sw_vers', ['-productVersion'], { encoding: 'utf8' }).trim()}`);
say(`windows measured with ${MEASURE === 'swift' ? 'a compiled Swift CGWindowList reader (not the companion\'s osascript probe)' : 'JXA through osascript (no swiftc)'}; errors in points and device pixels (scale 2)`);
say('');

// Install the companion from this build (no download: the app's own Electron dist).
const setup = spawnSyncLike(BIN, ['browser', 'setup', '--companion', '--yes']);
say(`setup --companion: ${setup.code === 0 ? 'ok' : 'FAILED'} ${setup.out.split('\n').filter((l) => /companion/.test(l)).slice(-1)[0] || ''}`);
function spawnSyncLike(cmd, args) {
  try {
    const out = execFileSync(cmd, args, { encoding: 'utf8', env, timeout: 300000, stdio: ['ignore', 'pipe', 'pipe'] });
    return { code: 0, out };
  } catch (e) {
    return { code: e.status || 1, out: `${e.stdout || ''}${e.stderr || ''}` };
  }
}

const results = [];
for (const name of WANT) {
  const v = VARIANTS[name];
  if (!v) continue;
  say(`\n## ${name}`);
  const r = await runVariant(name, v).catch((e) => ({ name, error: e.stack || String(e) }));
  results.push(r);
  if (r.error) say(`blocked: ${r.error}`);
}

async function runVariant(name, v) {
  const out = { name, server: null, ghostty: null };
  try {
    return await measureVariant(name, v, out);
  } finally {
    // every path ends the variant: the server (the companion and the client go with it)
    if (out.server && !out.stopped) await stopServer(out.server, out.ghostty, out);
  }
}

async function measureVariant(name, v, out) {
  // a fresh server per variant (the companion quits with it: lifetime tied to the server)
  const log = fs.openSync(path.join(T, `server-${name}.log`), 'a');
  const server = spawn(BIN, ['server'], { stdio: ['ignore', log, log], env, detached: false });
  out.server = server;
  if (!(await waitFor(() => sock() && api('browser.get', {}).then(() => true), 30000))) throw new Error('the isolated server did not come up');
  // the client's script, run by the Ghostty instance
  const marker = path.join(T, `marker-${name}`);
  const inner = path.join(T, `inner-${name}.sh`);
  const exports = Object.entries({ HOME: env.HOME, XDG_CONFIG_HOME: env.XDG_CONFIG_HOME, XDG_STATE_HOME: env.XDG_STATE_HOME, XDG_DATA_HOME: env.XDG_DATA_HOME, XDG_CACHE_HOME: env.XDG_CACHE_HOME })
    .map(([k, val]) => `export ${k}='${val}'`)
    .join('\n');
  fs.writeFileSync(inner, `#!/bin/bash\n${exports}\nfor v in $(env | sed -n 's/^\\(HERDR_[A-Za-z0-9_]*\\)=.*/\\1/p'); do unset "$v"; done\nexport HERDR_TEST_BROWSER_VIEW=side\necho $$ > '${marker}'\nexec '${BIN}'\n`, { mode: 0o755 });
  const before = new Set(ghosttyPids());
  execFileSync('/usr/bin/open', ['-g', '-n', '-a', GHOSTTY, '--args', '--quit-after-last-window-closed=true', '--window-save-state=never', '--auto-update=off', '--confirm-close-surface=false', `--window-width=${v.cols}`, `--window-height=${v.rows}`, '--window-position-x=160', '--window-position-y=120', ...v.args, `--initial-command=${inner}`]);
  const ghostty = await waitFor(() => ghosttyPids().find((p) => !before.has(p) && execFileSync('/bin/ps', ['-o', 'args=', '-p', String(p)], { encoding: 'utf8' }).includes(inner)), 10000);
  if (!ghostty) throw new Error('no new Ghostty instance');
  out.ghostty = ghostty;
  say(`Ghostty instance pid ${ghostty}: ${activate(ghostty)}`);
  if (!(await waitFor(() => fs.existsSync(marker), 10000))) throw new Error(`the script did not start in Ghostty ${ghostty} (left running, not killed)`);
  // the companion: started by the client's dock (a user start), managed by the server
  const port = await waitFor(async () => {
    const info = (await api('browser.get', {})).browser || {};
    const p = (info.profiles || []).find((x) => x.name === 'companion' && x.state === 'running');
    return p && p.port;
  }, 60000);
  if (!port) throw new Error('the companion did not register');
  const cdp = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 20000 });
  const control = cdp.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith('herdr-companion://control/'));
  const debug = () => control.evaluate(() => globalThis.__herdrCompanion.dock_debug());
  const covering = await waitFor(async () => {
    const d = await debug();
    return d && d.shown && d.decision && d.decision.reports.some((x) => x.state === 'covering') ? d : null;
  }, 30000);
  if (!covering) {
    const d = await debug().catch(() => null);
    await cdp.close().catch(() => {});
    throw new Error(`the dock did not cover: ${JSON.stringify(d && d.decision && d.decision.reports)}`);
  }
  await sleep(600); // settled
  const d = await debug();
  const companionPid = (await api('browser.companion', { action: 'status' })).companion.pid;
  const wins = windowsOf([ghostty, companionPid]);
  const term = wins.filter((w) => w.pid === ghostty).sort((a, b) => b.w * b.h - a.w * a.h)[0];
  const dock = wins.find((w) => w.pid === companionPid);
  const placement = d.decision.target.placement;
  const model = placementRect({ placement, frame: { x: term.x, y: term.y, w: term.w, h: term.h }, scale: 2, config: v.config });
  out.frame = term;
  out.dock = dock;
  out.grid = `${placement.grid.cols}x${placement.grid.rows}`;
  out.expected = model.rect;
  out.model = model.model;
  if (!dock || !model.ok) throw new Error(`no dock window on screen, or the model refused it: ${JSON.stringify({ dock, model })}`);
  const err = {
    left: dock.x - model.rect.x,
    top: dock.y - model.rect.y,
    right: dock.x + dock.w - (model.rect.x + model.rect.width),
    bottom: dock.y + dock.h - (model.rect.y + model.rect.height),
  };
  out.err = err;
  out.maxErrPt = Math.max(...Object.values(err).map(Math.abs));
  // the band's divider column, from the client's grid: the dock's left edge must meet it
  const cellW = placement.cell_px.width / 2;
  const padL = model.model.padL;
  out.dividerRight = term.x + padL + placement.area.x * cellW;
  out.leftVsDivider = dock.x - out.dividerRight;
  const above = aboveOf(term.num);
  out.directlyAbove = above.length > 0 && above[above.length - 1] === dock.num;
  // the probe's CPU over 10 s, docked and idle
  if (d.probe_pid) {
    const c0 = cpuSeconds(d.probe_pid);
    await sleep(10000);
    const c1 = cpuSeconds(d.probe_pid);
    out.probeCpuPct = Math.round(((c1 - c0) / 10) * 1000) / 10;
  }
  say(`grid ${placement.grid.cols}x${placement.grid.rows}, extent ${placement.extent_px.width}x${placement.extent_px.height} px, cell ${placement.cell_px.width}x${placement.cell_px.height} px, band cells x=${placement.area.x} w=${placement.area.width}`);
  say(`Ghostty frame ${term.x},${term.y} ${term.w}x${term.h} pt; model padL ${padL} pt, titlebar ${model.model.titlebar} pt`);
  say(`dock window ${dock.x},${dock.y} ${dock.w}x${dock.h} pt; expected ${model.rect.x},${model.rect.y} ${model.rect.width}x${model.rect.height}`);
  say(`error per edge (pt): left ${err.left}, top ${err.top}, right ${err.right}, bottom ${err.bottom}  → max ${out.maxErrPt} pt = ${out.maxErrPt * 2} px; left edge vs herdr's divider ${out.leftVsDivider.toFixed(2)} pt`);
  say(`error per edge (device px): left ${err.left * 2}, top ${err.top * 2}, right ${err.right * 2}, bottom ${err.bottom * 2}`);
  say(`z-order: directly above the Ghostty window: ${out.directlyAbove}; probe CPU ${out.probeCpuPct}% of a core`);
  await cdp.close().catch(() => {});
  return out;
}

async function stopServer(server, ghostty, out) {
  out.stopped = true;
  spawnSyncLike(BIN, ['server', 'stop']);
  await waitFor(() => server.exitCode !== null, 15000);
  if (!ghostty) return;
  const gone = await waitFor(() => !alive(ghostty), 20000);
  out.ghosttyQuit = !!gone;
  say(gone ? `Ghostty ${ghostty} quit after its window closed` : `Ghostty ${ghostty} still running (not killed; quit it from its Dock icon)`);
}

say('\n## Summary\n');
say('| variant | grid (cells) | Ghostty frame (pt) | dock window (pt) | max error | left vs divider | directly above | probe CPU | Ghostty quit |');
say('|---|---|---|---|---|---|---|---|---|');
for (const r of results) {
  if (r.error) {
    say(`| ${r.name} | | blocked | ${r.error.split('\n')[0].slice(0, 120)} | | | | | |`);
    continue;
  }
  say(`| ${r.name} | ${r.grid} | ${r.frame.w}x${r.frame.h} @${r.frame.x},${r.frame.y} | ${r.dock.w}x${r.dock.h} @${r.dock.x},${r.dock.y} | ${r.maxErrPt} pt (${r.maxErrPt * 2} px) | ${r.leftVsDivider.toFixed(2)} pt | ${r.directlyAbove} | ${r.probeCpuPct}% | ${r.ghosttyQuit} |`);
}
fs.writeFileSync(REPORT, lines.join('\n') + '\n');
console.log(`\nreport: ${REPORT}`);
const bad = results.filter((r) => r.error || r.maxErrPt > 1 || !r.directlyAbove);
process.exit(bad.length ? 1 : 0);
