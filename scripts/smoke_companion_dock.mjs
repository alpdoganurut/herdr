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
  // the defaults, then the z-order across "clicks" (clickSequence below)
  clicks: { args: [], cols: 120, rows: 36, config: {}, clicks: true },
  // the defaults, then a second native tab and switches between the tabs (tabSequence below);
  // the second tab runs cat, not a shell
  tabs: { args: ['--command=/bin/cat'], cols: 120, rows: 36, config: {}, tabs: true },
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
import CoreServices
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
        guard let pid = number(d, kCGWindowOwnerPID), pids.contains(pid), [0, 1].contains(number(d, kCGWindowLayer) ?? -1),
              let num = number(d, kCGWindowNumber), let bounds = d[kCGWindowBounds as String] as? NSDictionary,
              let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { continue }
        list.append(["num": num, "pid": pid, "x": rect.origin.x, "y": rect.origin.y, "w": rect.size.width, "h": rect.size.height])
    }
    out = list
} else if args.first == "watch", args.count > 5, let term = Int(args[1]), let dock = Int(args[2]), let helper = Int(args[3]),
          let ms = Double(args[4]), let seconds = Double(args[5]) {
    // The order around the terminal window every <ms> for <seconds>: per sample
    // [ms since 1970, dock above the terminal (0/1), the dock's layer (-1: not above), helper
    // above the terminal (0/1), helper in front of the dock (0/1)].
    var samples: [[Double]] = []
    let end = Date().timeIntervalSince1970 + seconds
    while Date().timeIntervalSince1970 < end {
        let above = windows([.optionOnScreenAboveWindow], CGWindowID(term))
        let t = Date().timeIntervalSince1970 * 1000
        let nums = above.map { number($0, kCGWindowNumber) ?? 0 }
        let di = nums.firstIndex(of: dock)
        let hi = nums.firstIndex(of: helper)
        let layer = di.map { Double(number(above[$0], kCGWindowLayer) ?? 0) } ?? -1
        samples.append([t, di == nil ? 0 : 1, layer, hi == nil ? 0 : 1, (hi != nil && (di == nil || hi! < di!)) ? 1 : 0])
        Thread.sleep(forTimeInterval: ms / 1000)
    }
    out = samples
} else if args.first == "click", args.count > 1, let pid = Int32(args[1]) {
    // Ghostty's "activate window" on the front window of the instance <pid> (its scripting
    // dictionary): the window ordered front and made key, its app active, as a click into it
    // does. Only with Automation already allowed (never prompts).
    let target = NSAppleEventDescriptor(processIdentifier: pid)
    guard AEDeterminePermissionToAutomateTarget(target.aeDesc, typeWildCard, typeWildCard, false) == noErr else {
        FileHandle.standardError.write(Data("no automation permission for Ghostty\\n".utf8)); exit(3)
    }
    func code(_ s: String) -> FourCharCode { s.utf8.reduce(0) { ($0 << 8) | FourCharCode($1) } }
    let ev = NSAppleEventDescriptor(eventClass: code("Ghst"), eventID: code("AcWn"), targetDescriptor: target, returnID: AEReturnID(kAutoGenerateReturnID), transactionID: AETransactionID(kAnyTransactionID))
    let spec = NSAppleEventDescriptor.record().coerce(toDescriptorType: DescType(typeObjectSpecifier))!
    spec.setDescriptor(NSAppleEventDescriptor(typeCode: DescType(typeProperty)), forKeyword: AEKeyword(keyAEDesiredClass))
    spec.setDescriptor(NSAppleEventDescriptor(enumCode: OSType(formPropertyID)), forKeyword: AEKeyword(keyAEKeyForm))
    spec.setDescriptor(NSAppleEventDescriptor(typeCode: code("GFWn")), forKeyword: AEKeyword(keyAEKeyData))
    spec.setDescriptor(NSAppleEventDescriptor.null(), forKeyword: AEKeyword(keyAEContainer))
    ev.setParam(spec, forKeyword: AEKeyword(keyDirectObject))
    out = ["ok": (try? ev.sendEvent(options: [.waitForReply], timeout: 5)) != nil]
} else if args.first == "vis", args.count > 4, let term = Int(args[1]), let dock = Int(args[2]),
          let ms = Double(args[3]), let seconds = Double(args[4]) {
    // Who is on screen every <ms> for <seconds>: per sample [ms since 1970, the terminal
    // window on screen (0/1), the dock on screen (0/1), the dock's layer (-1: off screen),
    // the dock above the terminal (0/1)].
    var samples: [[Double]] = []
    let end = Date().timeIntervalSince1970 + seconds
    while Date().timeIntervalSince1970 < end {
        let on = windows([.optionOnScreenOnly], kCGNullWindowID)
        let t = Date().timeIntervalSince1970 * 1000
        let nums = on.map { number($0, kCGWindowNumber) ?? 0 }
        let ti = nums.firstIndex(of: term)
        let di = nums.firstIndex(of: dock)
        let layer = di.map { Double(number(on[$0], kCGWindowLayer) ?? 0) } ?? -1
        samples.append([t, ti == nil ? 0 : 1, di == nil ? 0 : 1, layer, (ti != nil && di != nil && di! < ti!) ? 1 : 0])
        Thread.sleep(forTimeInterval: ms / 1000)
    }
    out = samples
} else if (args.first == "newtab" || args.first == "action"), args.count > 1, let pid = Int32(args[1]) {
    // Ghostty's "new tab" (in the front window), or "perform action <args[2]>" on the focused
    // terminal of the selected tab of the front window, for the instance <pid> only (its
    // scripting dictionary; only with Automation already allowed, never prompts).
    let target = NSAppleEventDescriptor(processIdentifier: pid)
    guard AEDeterminePermissionToAutomateTarget(target.aeDesc, typeWildCard, typeWildCard, false) == noErr else {
        FileHandle.standardError.write(Data("no automation permission for Ghostty\\n".utf8)); exit(3)
    }
    func code(_ s: String) -> FourCharCode { s.utf8.reduce(0) { ($0 << 8) | FourCharCode($1) } }
    func prop(_ c: String, _ container: NSAppleEventDescriptor) -> NSAppleEventDescriptor {
        let spec = NSAppleEventDescriptor.record().coerce(toDescriptorType: DescType(typeObjectSpecifier))!
        spec.setDescriptor(NSAppleEventDescriptor(typeCode: DescType(typeProperty)), forKeyword: AEKeyword(keyAEDesiredClass))
        spec.setDescriptor(NSAppleEventDescriptor(enumCode: OSType(formPropertyID)), forKeyword: AEKeyword(keyAEKeyForm))
        spec.setDescriptor(NSAppleEventDescriptor(typeCode: code(c)), forKeyword: AEKeyword(keyAEKeyData))
        spec.setDescriptor(container, forKeyword: AEKeyword(keyAEContainer))
        return spec
    }
    let ev: NSAppleEventDescriptor
    if args.first == "newtab" {
        ev = NSAppleEventDescriptor(eventClass: code("Ghst"), eventID: code("NTab"), targetDescriptor: target, returnID: AEReturnID(kAutoGenerateReturnID), transactionID: AETransactionID(kAnyTransactionID))
    } else {
        ev = NSAppleEventDescriptor(eventClass: code("Ghst"), eventID: code("PfAc"), targetDescriptor: target, returnID: AEReturnID(kAutoGenerateReturnID), transactionID: AETransactionID(kAnyTransactionID))
        ev.setParam(NSAppleEventDescriptor(string: args.count > 2 ? args[2] : ""), forKeyword: AEKeyword(keyDirectObject))
        ev.setParam(prop("GTfT", prop("GWsT", prop("GFWn", NSAppleEventDescriptor.null()))), forKeyword: code("GonT"))
    }
    out = ["ok": (try? ev.sendEvent(options: [.waitForReply], timeout: 5)) != nil]
} else if args.first == "quit", args.count > 1, let pid = Int32(args[1]) {
    // the standard "quit" Apple Event to the instance <pid> (its windows close without asking:
    // the instance runs with confirm-close-surface=false)
    let target = NSAppleEventDescriptor(processIdentifier: pid)
    let ev = NSAppleEventDescriptor(eventClass: AEEventClass(kCoreEventClass), eventID: AEEventID(kAEQuitApplication), targetDescriptor: target, returnID: AEReturnID(kAutoGenerateReturnID), transactionID: AETransactionID(kAnyTransactionID))
    out = ["ok": (try? ev.sendEvent(options: [.waitForReply], timeout: 5)) != nil]
} else if args.first == "top" {
    // the six frontmost normal windows: [number, pid]
    out = windows([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID).filter { number($0, kCGWindowLayer) == 0 }.prefix(6).map { [number($0, kCGWindowNumber) ?? 0, number($0, kCGWindowOwnerPID) ?? 0] }
} else if args.first == "above", args.count > 1, let wid = UInt32(args[1]) {
    // [number, layer], front to back
    out = windows([.optionOnScreenAboveWindow], CGWindowID(wid)).compactMap { d -> [Int]? in
        guard let num = number(d, kCGWindowNumber) else { return nil }
        return [num, number(d, kCGWindowLayer) ?? 0]
    }
} else {
    FileHandle.standardError.write(Data("usage: cgwin list <pid>... | cgwin above <wid>\\n".utf8))
    exit(2)
}
FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: out))
`;
// "Other app" for clickSequence: a plain AppKit window of its own process at a CG rect,
// shown without activating; it ends on "quit" or when its stdin closes.
const HELPER_SWIFT = `import AppKit
let a = CommandLine.arguments.dropFirst().compactMap { Double($0) }
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let screen = NSScreen.screens[0].frame
let win = NSWindow(contentRect: NSRect(x: a[0], y: screen.height - a[1] - a[3], width: a[2], height: a[3]), styleMask: [.titled], backing: .buffered, defer: false)
win.title = "herdr dock smoke: another app"
win.backgroundColor = .systemOrange
win.isReleasedWhenClosed = false
win.orderFront(nil)
print(win.windowNumber)
fflush(stdout)
DispatchQueue.global().async {
    while let line = readLine(), line != "quit" {
        if line == "front" {
            DispatchQueue.main.async {
                NSApp.activate(ignoringOtherApps: true)
                win.makeKeyAndOrderFront(nil)
                win.orderFrontRegardless()
            }
        }
    }
    DispatchQueue.main.async { app.terminate(nil) }
}
app.run()
`;

const CGWIN = path.join(T, 'cgwin');
let MEASURE = 'jxa';
try {
  fs.writeFileSync(`${CGWIN}.swift`, CGWIN_SWIFT);
  execFileSync('/usr/bin/swiftc', ['-O', '-o', CGWIN, `${CGWIN}.swift`], { stdio: 'ignore', timeout: 180000 });
  MEASURE = 'swift';
} catch {}
const cgwin = (args) => JSON.parse(execFileSync(CGWIN, args.map(String), { encoding: 'utf8', timeout: 10000 }));
// On-screen windows of `pids` at the normal level or the raised dock's (0, 1), front to back (CGWindowList: no permission).
function windowsOf(pids) {
  if (MEASURE === 'swift') return cgwin(['list', ...pids]);
  const src = `ObjC.import('CoreGraphics');
function run(argv) { const pids = argv.map(Number); const a = ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0)); const o = [];
for (let i = 0; i < a.count; i++) { const d = a.objectAtIndex(i); const pid = d.objectForKey('kCGWindowOwnerPID').js; if (!pids.includes(pid) || d.objectForKey('kCGWindowLayer').js > 1) continue;
const b = d.objectForKey('kCGWindowBounds'); o.push({ num: d.objectForKey('kCGWindowNumber').js, pid, x: b.objectForKey('X').js, y: b.objectForKey('Y').js, w: b.objectForKey('Width').js, h: b.objectForKey('Height').js }); }
return JSON.stringify(o); }`;
  return JSON.parse(jxa(src, pids));
}
function aboveOf(wid) {
  if (MEASURE === 'swift') return cgwin(['above', wid]);
  const src = `ObjC.import('CoreGraphics');
function run(argv) { const a = ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenAboveWindow, Number(argv[0]))); const o = [];
for (let i = 0; i < a.count; i++) { const d = a.objectAtIndex(i); o.push([d.objectForKey('kCGWindowNumber').js, d.objectForKey('kCGWindowLayer').js]); } return JSON.stringify(o); }`;
  return JSON.parse(jxa(src, [wid]));
}
// Brings the app `pid` to the front, only when its executable is `exe` (a test instance's
// Ghostty, the clicks helper): `options` 2 = ignoring other apps, 3 = and all its windows.
function activate(pid, exe = GHOSTTY_EXE, options = 2) {
  const src = `ObjC.import('AppKit'); function run(argv) { const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
if (app.isNil()) return 'gone'; if (app.executableURL.path.js !== argv[1]) return 'not the test app'; return String(app.activateWithOptions(Number(argv[2]))); }`;
  return jxa(src, [pid, exe, options]);
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
  // directly above: the dock is above the terminal (at any level) with no normal window
  // between them
  const at = above.findIndex(([num]) => num === dock.num);
  out.directlyAbove = at >= 0 && !above.slice(at + 1).some(([, layer]) => layer === 0);
  out.dockLayer = at >= 0 ? above[at][1] : null;
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
  say(`z-order: directly above the Ghostty window: ${out.directlyAbove} (dock layer ${out.dockLayer}); probe CPU ${out.probeCpuPct}% of a core`);
  if (v.clicks) out.clicks = await clickSequence({ ghostty, term, dock, debug });
  if (v.tabs) out.tabs = await tabSequence({ ghostty, term, dock, debug });
  await cdp.close().catch(() => {});
  return out;
}

// The z-order across "clicks". No permission posts input events here, so a click is what it
// does to the window order: Ghostty's own "activate window" (Apple Events to the test instance
// only; skipped when Automation is not already allowed: never prompts) orders the terminal's
// window front and makes its app active, and another app (a helper window over half of the
// dock) comes to the front by itself. A watcher samples the order every 2 ms. Must hold: the
// dock above the terminal in every sample (no flicker, no hide/show), and the other app's
// window in front of the dock once it is frontmost (other apps cover both).
async function clickSequence({ ghostty, term, dock, debug }) {
  if (MEASURE !== 'swift') throw new Error('the clicks need the compiled Swift reader (swiftc)');
  const res = { events: [] };
  const helperBin = path.join(T, 'helper');
  fs.writeFileSync(`${helperBin}.swift`, HELPER_SWIFT);
  execFileSync('/usr/bin/swiftc', ['-O', '-o', helperBin, `${helperBin}.swift`], { stdio: 'ignore', timeout: 180000 });
  const hx = Math.round(dock.x + dock.w / 2);
  const hy = Math.round(dock.y + 60);
  const helper = spawn(helperBin, [hx, hy, 320, 220].map(String), { stdio: ['pipe', 'pipe', 'ignore'] });
  let helperOut = '';
  helper.stdout.on('data', (d) => (helperOut += d));
  const helperWid = Number(await waitFor(() => helperOut.trim(), 5000, 50));
  const helperDecl = helperWid ? null : 'no helper window';
  const shownBefore = (await debug()).shown;
  const ev = (what, fn) => {
    const t = Date.now();
    const r = fn();
    res.events.push({ t, what, r });
    setTimeout(() => {
      try {
        res.events.find((e) => e.t === t && e.what === what).top = cgwin(['top']);
      } catch {}
    }, 150);
  };
  const watchSeconds = 13;
  let watchOut = '';
  const watcher = spawn(CGWIN, ['watch', term.num, dock.num, helperWid || 0, 2, watchSeconds].map(String), { stdio: ['ignore', 'pipe', 'ignore'] });
  watcher.stdout.on('data', (d) => (watchOut += d));
  const click = () => {
    try {
      return JSON.parse(execFileSync(CGWIN, ['click', ghostty], { encoding: 'utf8', timeout: 10000 })).ok;
    } catch (e) {
      return `failed (${String(e.stderr || e.message).trim()})`;
    }
  };
  await sleep(300);
  for (let i = 0; i < 6; i++) {
    ev('click', click);
    await sleep(600);
  }
  for (let i = 0; i < 4; i++) {
    ev('other app', () => (helper.stdin.write('front\n'), true));
    await sleep(900);
    ev('click', click);
    await sleep(900);
  }
  await waitFor(() => watcher.exitCode !== null, (watchSeconds + 5) * 1000, 100);
  helper.stdin.end('quit\n');
  const shownAfter = (await debug()).shown;
  let samples = [];
  try {
    samples = JSON.parse(watchOut);
  } catch {}
  // each sample against the last event before it
  const first = res.events[0].t;
  let below = 0;
  let belowMs = 0;
  let overOther = 0;
  let overOtherEarly = 0;
  const layers = new Set();
  let prevT = null;
  for (const [t, dockAbove, layer, , helperFront] of samples) {
    if (t < first) continue;
    const e = res.events.filter((x) => x.t <= t).at(-1);
    if (!dockAbove) {
      below += 1;
      if (prevT !== null) belowMs += t - prevT;
    } else layers.add(layer);
    if (e && e.what === 'other app' && dockAbove && !helperFront) {
      if (t - e.t > 300) overOther += 1;
      else overOtherEarly += 1;
    }
    prevT = t;
  }
  fs.writeFileSync(`${REPORT}.clicks.json`, JSON.stringify({ term: term.num, dock: dock.num, helper: helperWid, events: res.events, samples }));
  res.samples = samples.filter(([t]) => t >= first).length;
  res.below = below;
  res.belowMs = Math.round(belowMs);
  res.overOther = overOther;
  res.overOtherEarly = overOtherEarly;
  res.layers = [...layers];
  res.shown = { before: shownBefore, after: shownAfter };
  res.ok = !helperDecl && res.samples > 1000 && below === 0 && overOther === 0 && shownBefore && shownAfter;
  say(`clicks: ${res.events.length} activations (${res.events.map((e) => `${e.what}=${e.r}`).filter((x, i, a) => a.indexOf(x) === i).join(', ')}), ${res.samples} order samples every 2 ms${helperDecl ? `; ${helperDecl}` : ''}`);
  say(`clicks: dock below the terminal in ${below} samples (~${res.belowMs} ms); dock in front of the frontmost other app's window in ${overOther} samples after 300 ms (${overOtherEarly} within 300 ms of its activation); dock layers seen ${res.layers.join('/')}; shown before/after ${shownBefore}/${shownAfter}`);
  return res;
}

// Ghostty's native tabs: each tab is a window of its own at the same frame, only the selected
// one on screen. A second tab (Ghostty's "new tab", Apple Events to the test instance only;
// skipped when Automation is not already allowed: never prompts), then switches between the
// two ("goto_tab:N") and clicks into tab 1, while a watcher samples which windows are on
// screen every 2 ms. Must hold: the dock never on screen while the terminal's tab is not
// (beyond the probe's sample period), back on screen once tab 1 is selected again, and above
// the terminal whenever both are on screen.
async function tabSequence({ ghostty, term, dock, debug }) {
  if (MEASURE !== 'swift') throw new Error('the tabs need the compiled Swift reader (swiftc)');
  const res = { events: [] };
  const send = (...args) => {
    try {
      return JSON.parse(execFileSync(CGWIN, args.map(String), { encoding: 'utf8', timeout: 10000 })).ok;
    } catch (e) {
      return `failed (${String(e.stderr || e.message).trim()})`;
    }
  };
  const ev = (what, fn) => res.events.push({ t: Date.now(), what, r: fn() });
  const watchSeconds = 16;
  let watchOut = '';
  const watcher = spawn(CGWIN, ['vis', term.num, dock.num, 2, watchSeconds].map(String), { stdio: ['ignore', 'pipe', 'ignore'] });
  watcher.stdout.on('data', (d) => (watchOut += d));
  await sleep(500);
  ev('new tab', () => send('newtab', ghostty));
  await sleep(2000);
  for (let i = 0; i < 3; i++) {
    ev('tab 1', () => send('action', ghostty, 'goto_tab:1'));
    await sleep(1200);
    ev('click', () => send('click', ghostty));
    await sleep(800);
    ev('tab 2', () => send('action', ghostty, 'goto_tab:2'));
    await sleep(1500);
  }
  ev('tab 1', () => send('action', ghostty, 'goto_tab:1'));
  await waitFor(() => watcher.exitCode !== null, (watchSeconds + 5) * 1000, 100);
  const after = await debug();
  let samples = [];
  try {
    samples = JSON.parse(watchOut);
  } catch {}
  fs.writeFileSync(`${REPORT}.tabs.json`, JSON.stringify({ term: term.num, dock: dock.num, events: res.events, samples }));
  const first = res.events[0].t;
  // runs of the terminal's tab off screen: how long the dock stayed on screen into each, and
  // how long it took to come back after each
  let overOther = 0; // samples with the dock on screen while the terminal is not
  let below = 0; // both on screen, the dock not above the terminal
  const hideLag = [];
  const showLag = [];
  let prev = null;
  let since = null; // when the terminal's last on/off change happened
  let pending = null; // 'hide' | 'show': waiting for the dock to follow
  for (const [t, termOn, dockOn, , dockAbove] of samples) {
    if (t < first) continue;
    if (prev !== null && termOn !== prev) {
      since = t;
      pending = termOn ? 'show' : 'hide';
    }
    prev = termOn;
    if (!termOn && dockOn) overOther += 1;
    if (termOn && dockOn && !dockAbove) below += 1;
    if (pending === 'hide' && !dockOn) {
      hideLag.push(Math.round(t - since));
      pending = null;
    } else if (pending === 'show' && dockOn) {
      showLag.push(Math.round(t - since));
      pending = null;
    }
  }
  res.samples = samples.filter(([t]) => t >= first).length;
  res.offRuns = hideLag.length + (pending === 'hide' ? 1 : 0);
  res.hideLag = hideLag;
  res.showLag = showLag;
  res.overOther = overOther;
  res.below = below;
  res.shownAfter = after.shown;
  const failed = res.events.filter((e) => e.r !== true);
  res.ok = !failed.length && res.samples > 1000 && hideLag.length >= 3 && showLag.length >= 3 && pending === null && Math.max(...hideLag) <= 250 && Math.max(...showLag) <= 500 && below === 0 && after.shown;
  say(`tabs: ${res.events.map((e) => `${e.what}=${e.r}`).join(', ')}; ${res.samples} on-screen samples every 2 ms`);
  say(`tabs: the dock left the screen ${hideLag.length} times after its tab did (lag ms ${hideLag.join('/')}), came back ${showLag.length} times (lag ms ${showLag.join('/')}); on screen without its tab in ${overOther} samples; below the terminal in ${below}; shown at the end ${after.shown}`);
  return res;
}

async function stopServer(server, ghostty, out) {
  out.stopped = true;
  spawnSyncLike(BIN, ['server', 'stop']);
  await waitFor(() => server.exitCode !== null, 15000);
  if (!ghostty) return;
  let gone = await waitFor(() => !alive(ghostty), out.tabs ? 5000 : 20000);
  // the tabs variant's second tab (cat) keeps the instance: asked to quit, its own instance only
  if (!gone && out.tabs && MEASURE === 'swift' && execFileSync('/bin/ps', ['-o', 'args=', '-p', String(ghostty)], { encoding: 'utf8' }).includes(T)) {
    try {
      execFileSync(CGWIN, ['quit', ghostty], { timeout: 10000 });
    } catch {}
    gone = await waitFor(() => !alive(ghostty), 15000);
  }
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
const bad = results.filter((r) => r.error || r.maxErrPt > 1 || !r.directlyAbove || (r.clicks && !r.clicks.ok) || (r.tabs && !r.tabs.ok));
process.exit(bad.length ? 1 : 0);
