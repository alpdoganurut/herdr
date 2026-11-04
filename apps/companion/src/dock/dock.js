// The docked companion (launcher design §3.4): a pure core that decides, and an Electron
// adapter that does what it decided.
//
// herdr publishes placements (dock/placements.js); the window probe (dock/window-probe.js)
// says where the terminal windows are. The core binds the target placement to its terminal
// window (the client's process ancestry owns it; with several, the frontmost when the
// client's focus flips or the user clicks the "unbound" placeholder, from a window list
// newer than that), follows it, and says when the dock window shows and where
// (dock/geometry.js), and what to report to herdr:
//   covering    over the area
//   hidden      not now (the placement is not shown, or the window is off screen: minimised,
//               another Space, or a Ghostty tab that is not the selected one; or the probe
//               is down, so where the window is is not known)
//   moving      the window moves or resizes: hidden until it settles (120 ms)
//   unbound     several terminal windows could be this client's: one click binds it
//   unsupported with a reason the user can act on
//   elsewhere   another client's placement is the target
// "Covered" is handled by z-order: the dock sits directly above the terminal window, so
// whatever covers the terminal covers the dock the same way.
// The dock is another app's window: at the same (normal) level, a click into the terminal
// orders the terminal's window front, over the dock, until the dock is ordered back above it
// (a visible flicker on every click). So while the terminal's app (or the companion) is the
// frontmost app and no other normal window is above the terminal, the dock sits one level
// above normal windows ("raised"): a click into the terminal cannot cover it, and it takes
// neither focus nor activation. Once another app comes to the front (the workspace's
// activation notice, at once) or another window gets above the terminal (the next sample),
// it is back at the normal level directly above the terminal, so other windows cover both.
// Windows of other processes cannot be parent and child (addChildWindow is in-process only).
// The user brings the companion to the front (a click into the dock while another app is in
// front, `browser companion focus`) while other windows are above the terminal: the terminal's
// window comes along ("paired"), ordered front by its own app's scripting without activating it
// (dock/front.jxa.js: an inactive app's window goes just below the active app's, so the pair
// sits together above the others and the keyboard stays in the dock); meanwhile the dock stays
// raised instead of dropping back under those windows. Once per activation; never for an
// agent (an agent's focus never activates the companion).

'use strict';

const { execFile } = require('node:child_process');
const { placementRect, isFullScreen, REASONS } = require('./geometry.js');
const { chooseTarget, anyDockPlacement } = require('./placements.js');
const { readGhosttyConfig } = require('./ghostty-config.js');
const path = require('node:path');

const FRONT = path.join(__dirname, 'front.jxa.js');

const SETTLE_MS = 120;
// Right after a move or resize herdr's placement may still describe the old size: a
// geometry that does not fit is "moving" this long before it is "unsupported".
const RESETTLE_MS = 1500;
// The Dock icon comes back this long after the last placement went.
const POLICY_GRACE_MS = 2000;
// `configFor` while a terminal's settings are still being read: the dock waits (hidden).
const CONFIG_PENDING = Symbol('config pending');
// A geometry that reads as a split may be a settings change instead (a Ghostty config reload
// changes the padding): the settings are read again, at most this often per terminal.
const CONFIG_RECHECK_MS = 5000;
// The placement's terminal extent or cell size changed (a resize, a font change, or a config
// reload: a changed vertical padding still gives a valid rect, only a wrong one): the
// settings are read again, at most this often per terminal.
const CONFIG_FOLLOW_MS = 1000;

// What a terminal's settings were checked against: the placement's pixel extent and cell size.
const geometryKey = (placement) => {
  const e = placement.extent_px || {};
  const c = placement.cell_px || {};
  return `${e.width}x${e.height}/${c.width}x${c.height}`;
};

// The terminal's geometry settings for `pid`: Ghostty's config files and its own flags (they
// win; `ps`, no permission); another terminal is read as evenly padded (best effort).
// `ghostty`: whether the terminal is Ghostty (only its window can come along, front.jxa.js).
function readTerminalSettings(pid, fake, done) {
  execFile('/bin/ps', ['-o', 'args=', '-p', String(pid)], { encoding: 'utf8', timeout: 2000 }, (err, stdout) => {
    const args = err ? '' : String(stdout || '').trim();
    const ghostty = !!fake || /ghostty/i.test(args.split(/\s+/)[0] || '');
    done(ghostty ? { ...readGhosttyConfig({ argv: args.split(/\s+/).slice(1) }), ghostty } : { symmetric: true, sources: [], ghostty });
  });
}

// A sample taken this soon after the frontmost app changed may still show the order from
// before: it does not take the dock off the raised level.
const FRONT_SETTLE_MS = 50;
// After the terminal's window was asked to come along with the companion: the dock stays
// raised this long while the order catches up (or the ask failed: then back under the others).
const PAIR_MS = 800;
// The level above normal windows: one above (other apps' floating panels stay above it).
const RAISED = ['normal', 1];

// The pid in a workspace activation notice's userInfo (Electron gives the
// NSRunningApplication's description: "<NSRunningApplication: 0x… (com.mitchellh.ghostty - 659) …>").
function activatedPid(userInfo) {
  const app = userInfo && userInfo.NSWorkspaceApplicationKey;
  const m = /\s-\s(\d+)\)/.exec(String(app || ''));
  return m ? Number(m[1]) : null;
}

// The frontmost app's pid (`lsappinfo`, no permission), or null.
function readFrontPid(done) {
  execFile('/usr/bin/lsappinfo', ['front'], { encoding: 'utf8', timeout: 2000 }, (err, asn) => {
    const id = err ? '' : String(asn || '').trim();
    if (!/^ASN:/.test(id)) return done(null);
    execFile('/usr/bin/lsappinfo', ['info', '-only', 'pid', id], { encoding: 'utf8', timeout: 2000 }, (err2, out) => {
      const m = err2 ? null : /"pid"\s*=\s*(\d+)/.exec(String(out || ''));
      done(m ? Number(m[1]) : null);
    });
  });
}

const NO_WINDOW = 'Docking needs herdr directly in a terminal window (not inside tmux or ssh)';
const NOT_MAC = 'Docking is macOS only for now';

const sameFrame = (a, b) => !!a && !!b && a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;

class DockCore {
  // env: {platform, scaleFor(frame) -> number, displayFor(frame) -> {x,y,width,height},
  //       configFor(pid, key) -> ghostty config | CONFIG_PENDING (`key`: the placement's
  //         extent and cell size, geometryKey),
  //       configSuspect?(pid, updatedUnix) -> whether the settings are being read again,
  //       selfPid?: the companion's pid}
  constructor(env) {
    this.env = env;
    this.placements = [];
    this.bound = null; // {wid, pid, placementId}
    this.seen = new Map(); // placement id -> {focused, nudge}
    // placement id -> the candidate list count when its focus flipped or its nudge changed:
    // the rebind waits for a newer list (the one before the click has the old window in front)
    this.rebind = new Map();
    this.sample = null;
    this.candidates = null; // last candidate list (null: none seen yet)
    this.candidatesSeq = 0; // candidate lists seen
    this.frame = null; // the bound window's last frame
    this.frameSince = 0; // when it last changed
    this.movedAt = 0; // the last change (for RESETTLE_MS)
    this.dockWid = null; // our dock window's number (z-order upkeep)
    this.front = null; // {pid, at}: the frontmost app (null: not known yet)
    this.pairedFor = null; // the activation (front.at) the terminal's window was asked for
    this.pairAt = 0; // when (0: no ask in flight or it did not bring the window: no raised hold)
    this.pairOff = false; // the terminal's app refused (no Automation), or it missed: never asked again
    this.pairMissed = false; // an ask took the terminal's window off screen
  }

  // The frontmost app changed (the workspace's notice, or read once at the start).
  setFront(pid, now) {
    this.front = pid ? { pid, at: now } : null;
  }

  setPlacements(list) {
    this.placements = list || [];
    const ids = new Set();
    for (const p of this.placements) {
      const id = p.placement && p.placement.id;
      if (!id) continue;
      ids.add(id);
      const was = this.seen.get(id) || { focused: false, nudge: 0 };
      const nudge = p.placement.nudge || 0;
      if ((p.focused && !was.focused) || nudge !== was.nudge) this.rebind.set(id, this.candidatesSeq);
      this.seen.set(id, { focused: !!p.focused, nudge });
    }
    for (const id of [...this.seen.keys()]) if (!ids.has(id)) this.seen.delete(id);
    for (const id of [...this.rebind.keys()]) if (!ids.has(id)) this.rebind.delete(id);
    if (this.bound && !ids.has(this.bound.placementId)) this.bound = null;
  }

  onSample(sample, now) {
    this.sample = sample;
    if (sample.candidates) {
      this.candidates = sample.candidates;
      this.candidatesSeq += 1;
    }
    const t = sample.target;
    // Right after the terminal's window was asked to come along, it went off screen: the ask
    // reached another window (Ghostty's "front window" can be on another Space). Never again.
    if (this.pairAt && !this.pairOff && this.bound && t && t.num === this.bound.wid && t.on === false && now - this.pairAt < PAIR_MS) {
      this.pairOff = true;
      this.pairMissed = true;
    }
    if (this.bound && t && t.num === this.bound.wid) {
      if (!sameFrame(t, this.frame)) {
        this.frame = { x: t.x, y: t.y, w: t.w, h: t.h };
        this.frameSince = now;
        this.movedAt = now;
      }
    }
  }

  // The geometry of `placement` over the window `win` (a probe window record); `pending`
  // while the window's terminal settings are still being read.
  rectFor(placement, win) {
    const config = this.env.configFor(win.pid, geometryKey(placement));
    if (config === CONFIG_PENDING) return { ok: false, pending: true, model: {} };
    const frame = { x: win.x, y: win.y, w: win.w, h: win.h };
    return placementRect({ placement, frame, scale: this.env.scaleFor(frame), config });
  }

  // A split reason may be stale settings: whether they are being read again for `pid`.
  _rechecking(pid, target) {
    return !!(this.env.configSuspect && this.env.configSuspect(pid, target.updated_unix || 0));
  }

  // Bind the target to its window; returns {state, reason} when it cannot.
  _bind(target) {
    const placement = target.placement;
    const pids = placement.pids || [];
    // A rebind asked for (the focus flipped, a nudge) is answered from a candidate list
    // newer than the ask: the one from before the click still has the old window in front.
    const asked = this.rebind.has(placement.id);
    const fresh = asked && this.candidatesSeq > this.rebind.get(placement.id);
    if (this.bound && this.bound.placementId === placement.id) {
      const cand = (this.candidates || []).find((w) => w.num === this.bound.wid);
      const t = this.sample && this.sample.target;
      const live = cand || (t && t.num === this.bound.wid);
      // Keep the window until it leaves the list or another one is asked for.
      if (live && !fresh) return null;
    }
    if (this.candidates === null) return { state: 'hidden' }; // nothing seen yet
    const mine = this.candidates.filter((w) => pids.includes(w.pid));
    if (!mine.length) {
      this.bound = null;
      return { state: 'unsupported', reason: NO_WINDOW };
    }
    const geos = mine.map((w) => this.rectFor(placement, w));
    if (geos.some((g) => g.pending)) return { state: 'hidden' }; // reading the terminal's settings
    const fits = mine.filter((w, i) => geos[i].ok);
    if (!fits.length) {
      // the client's windows exist but none matches its grid: say why
      const reason = geos[0].reason || REASONS.split;
      if (reason === REASONS.split && this._rechecking(mine[0].pid, target)) return { state: 'moving' };
      this.bound = null;
      return { state: 'unsupported', reason };
    }
    let pick = null;
    if (fresh) pick = fits[0]; // front to back: the frontmost
    else if (fits.length === 1) pick = fits[0];
    if (fresh) this.rebind.delete(placement.id);
    if (!pick) {
      if (this.bound && fits.some((w) => w.num === this.bound.wid)) return null;
      this.bound = null;
      // waiting for the list that answers the ask: calm, not "click here"
      return { state: asked ? 'hidden' : 'unbound' };
    }
    if (!this.bound || this.bound.wid !== pick.num) {
      this.bound = { wid: pick.num, pid: pick.pid, placementId: placement.id };
      this.frame = { x: pick.x, y: pick.y, w: pick.w, h: pick.h };
      this.frameSince = 0; // a window seen in the candidate list is settled
    }
    return null;
  }

  // What to do now. Returns
  //   {docked, show, rect, fullScreen, level, raise, target, tabId, watch: {pids, wid},
  //    reports: [{id, state, reason}], recheckAt}
  // `level`: 'raised' (above normal windows: the terminal's app or the companion is frontmost
  // and nothing else is above the terminal) or 'normal' (directly above the terminal; `raise`:
  // ordered there again).
  // `recheckAt`: while the dock waits for a window to settle, when the decision changes
  // without a new sample or placement (null otherwise).
  decide(now) {
    const out = { docked: anyDockPlacement(this.placements), show: false, rect: null, fullScreen: false, level: 'normal', raise: false, target: null, tabId: null, watch: { pids: [], wid: 0 }, reports: [], recheckAt: null };
    const target = chooseTarget(this.placements);
    out.target = target;
    for (const p of this.placements) {
      if (p !== target && p.placement && p.placement.id) out.reports.push({ id: p.placement.id, state: target ? 'elsewhere' : 'hidden' });
    }
    if (!target) return out;
    const placement = target.placement;
    out.tabId = placement.tab_id;
    const say = (state, reason) => {
      out.reports.push({ id: placement.id, state, ...(reason ? { reason } : {}) });
      return out;
    };
    if (this.env.platform !== 'darwin' && !this.env.fake) return say('unsupported', NOT_MAC);
    const px = placement.extent_px || {};
    if (!(px.width > 0) || !(px.height > 0)) return say('unsupported', REASONS.noPixels);
    out.watch = { pids: placement.pids || [], wid: 0 };
    const why = this._bind(target);
    if (why) return say(why.state, why.reason);
    out.watch.wid = this.bound.wid;
    // The probe is down (restarting): where the window is is not known, so not shown.
    if (this.sample && this.sample.lost) return say('hidden');
    const t = this.sample && this.sample.target;
    // Off screen: minimised, another Space, the app hidden, or another tab of its tab group
    // selected (a Ghostty tab is a window of its own; only the selected one is on screen).
    if (t && t.num === this.bound.wid && t.on === false) return say('hidden');
    // The probe watching the window finds it nowhere (closed): not shown where it was; the
    // next candidate list without it binds again.
    if (this.sample && this.sample.wid === this.bound.wid && !t) return say('hidden');
    if (!this.frame) return say('hidden');
    if (this.frameSince && now - this.frameSince < SETTLE_MS) {
      out.recheckAt = this.frameSince + SETTLE_MS;
      return say('moving');
    }
    const win = { ...this.frame, pid: this.bound.pid };
    const geo = this.rectFor(placement, win);
    if (geo.pending) return say('hidden');
    if (!geo.ok) {
      if (this.movedAt && now - this.movedAt < RESETTLE_MS) {
        out.recheckAt = this.movedAt + RESETTLE_MS;
        return say('moving');
      }
      if (geo.reason === REASONS.split && this._rechecking(this.bound.pid, target)) return say('moving');
      return say('unsupported', geo.reason);
    }
    out.show = true;
    out.rect = geo.rect;
    out.model = geo.model;
    out.fullScreen = isFullScreen(this.frame, this.env.displayFor(this.frame));
    // Directly above the terminal window: the last layer-0 window of its above-list.
    const above = ((this.sample && this.sample.above) || []).filter((w) => (w[2] || 0) === 0);
    const last = above.length ? above[above.length - 1][0] : null;
    out.pair = this._pair(above, now);
    out.level = this._raised(above, now) ? 'raised' : 'normal';
    out.raise = out.level === 'normal' && (this.dockWid === null || last !== this.dockWid);
    return say('covering');
  }

  // Raised: the terminal's app or the companion is the frontmost app, and no other normal
  // window is above the terminal (a sample from right after the app came to the front may
  // still show the app before it: until a later one, the dock stays raised).
  _raised(above, now) {
    const f = this.front;
    if (!f || !this.bound || (f.pid !== this.bound.pid && f.pid !== this.env.selfPid)) return false;
    if (!above.some((w) => w[0] !== this.dockWid)) return true;
    if (f.pid === this.env.selfPid && this.pairedFor === f.at && this.pairAt && now - this.pairAt < PAIR_MS) return true;
    const t = this.sample && typeof this.sample.t === 'number' ? this.sample.t : 0;
    return t < f.at + FRONT_SETTLE_MS;
  }

  // The companion came to the front with other windows above the terminal: the terminal's
  // window, to come along ({pid, rank, onscreen}: its place among its app's on-screen windows,
  // front to back: the rank from this sample's above-list, the count from the last candidate
  // list), once per activation; else null. Ghostty only (its scripting does it).
  _pair(above, now) {
    const f = this.front;
    if (this.pairOff || !f || f.pid !== this.env.selfPid || !this.bound || !this.dockWid) return null;
    if (this.pairedFor === f.at || !above.some((w) => w[0] !== this.dockWid)) return null;
    const cfg = this.env.configFor(this.bound.pid);
    if (!cfg || cfg === CONFIG_PENDING || !cfg.ghostty) return null;
    const mine = (this.candidates || []).filter((w) => w.pid === this.bound.pid);
    if (!mine.some((w) => w.num === this.bound.wid)) return null;
    this.pairedFor = f.at;
    this.pairAt = now;
    return { pid: this.bound.pid, rank: 1 + above.filter((w) => w[1] === this.bound.pid).length, onscreen: mine.length };
  }

  // The ask for the terminal's window came back (`status`, orderFrontTerminal): when it did not
  // come along, the dock does not stay raised over the other windows for the rest of PAIR_MS.
  // 'denied': never asked again.
  pairDone(status) {
    if (status === 'ok') return;
    this.pairAt = 0;
    if (status === 'denied') this.pairOff = true;
  }
}

// front.jxa.js's exit as a status (`err`: execFile's error, null for exit 0): 'ok',
// 'skipped' (4: not sure which window it is), 'denied' (5: no Automation), else 'failed'
// (another exit, a signal, a timeout kill, a spawn error).
function pairStatus(err) {
  if (!err) return 'ok';
  if (err.signal || err.killed) return 'failed';
  return err.code === 4 ? 'skipped' : err.code === 5 ? 'denied' : 'failed';
}

// The first ask may wait for the user to answer macOS's Automation prompt.
const FRONT_TIMEOUT_MS = 30000;

// The terminal's window to the front with the companion (front.jxa.js); `done(status, why)`
// (pairStatus).
function orderFrontTerminal({ pid, rank, onscreen }, done) {
  execFile('/usr/bin/osascript', ['-l', 'JavaScript', FRONT, String(pid), String(rank), String(onscreen)], { encoding: 'utf8', timeout: FRONT_TIMEOUT_MS }, (err, stdout, stderr) => {
    done(pairStatus(err), String(stderr || '').trim());
  });
}

// The Electron side. `host` is the companion (main.js): views, windows, strip, groups.
class DockController {
  // `readSettings(pid, fake, done)`: the terminal's settings (tests replace it).
  // `readFront(done)`: the frontmost app's pid (tests replace it).
  // `orderFront(pair, done)`: the terminal's window to the front with the companion
  // (orderFrontTerminal; none headless or with fake windows; tests replace it).
  constructor({ host, feed, probe, log = () => {}, headless = false, platform = process.platform, electron, readSettings = readTerminalSettings, readFront = readFrontPid, selfPid = process.pid, orderFront }) {
    this.host = host;
    this.feed = feed;
    this.probe = probe;
    this.log = log;
    this.headless = headless;
    this.electron = electron;
    this.readSettings = readSettings;
    this.readFront = readFront;
    this.orderFront = orderFront !== undefined ? orderFront : headless || probe.fakeFile || platform !== 'darwin' ? null : orderFrontTerminal;
    const { screen } = electron;
    // pid -> {cfg, at, reading, key: the geometryKey they were read for, seen: the last one
    // asked about, timer: a re-read waiting for CONFIG_FOLLOW_MS}: the terminal's settings
    this.configs = new Map();
    this.core = new DockCore({
      platform,
      fake: !!probe.fakeFile,
      scaleFor: (frame) => {
        if (host.fakeScale) return host.fakeScale;
        try {
          return screen.getDisplayMatching({ x: Math.round(frame.x), y: Math.round(frame.y), width: Math.max(1, Math.round(frame.w)), height: Math.max(1, Math.round(frame.h)) }).scaleFactor || 1;
        } catch {
          return 1;
        }
      },
      displayFor: (frame) => {
        try {
          return screen.getDisplayMatching({ x: Math.round(frame.x), y: Math.round(frame.y), width: Math.max(1, Math.round(frame.w)), height: Math.max(1, Math.round(frame.h)) }).bounds;
        } catch {
          return null;
        }
      },
      configFor: (pid, key) => this.configFor(pid, key),
      configSuspect: (pid, updatedUnix) => this.configSuspect(pid, updatedUnix),
      selfPid,
    });
    this.docked = false;
    this.shown = false;
    this.stopped = false;
    this.last = null; // the last decision
    this.fullScreen = false;
    this.policyTimer = null;
    this.settleTimer = null;
    this.accessory = false;
    this.level = 'normal'; // the dock window's level (decide().level)
    this.frontSub = null;
    // The frontmost app, the moment it changes (no permission needed).
    const prefs = electron.systemPreferences;
    if (!headless && platform === 'darwin' && prefs && typeof prefs.subscribeWorkspaceNotification === 'function') {
      try {
        this.frontSub = prefs.subscribeWorkspaceNotification('NSWorkspaceDidActivateApplicationNotification', (_event, userInfo) => this.onFront(activatedPid(userInfo)));
      } catch (e) {
        this.log('dock: no activation notices', e.message);
      }
    }
    feed.on('placements', (list) => {
      this.core.setPlacements(list);
      this.apply();
    });
    probe.on('sample', (s) => {
      this.core.onSample(s, Date.now());
      this.apply();
    });
    // The probe died (restarting after a pause): no samples meanwhile, so the dock hides
    // rather than staying where the window was.
    probe.on('lost', () => {
      this.core.onSample({ t: Date.now(), lost: true }, Date.now());
      this.apply();
    });
  }

  // Another app is frontmost (`pid`): the dock's level follows at once.
  onFront(pid) {
    if (this.stopped || !pid) return;
    this.core.setFront(pid, Date.now());
    this.apply();
  }

  // The terminal's geometry settings (readTerminalSettings), read in the background:
  // CONFIG_PENDING until then (the dock waits). `key` (geometryKey): when the placement's
  // extent or cell size is not the one they were read for, they are read again, at most every
  // CONFIG_FOLLOW_MS; the old ones serve until the read lands, which decides again.
  configFor(pid, key = null) {
    const entry = this.configs.get(pid);
    if (entry && entry.cfg) {
      if (key !== null) {
        entry.seen = key;
        if (entry.key !== key) this.followConfig(pid, entry);
      }
      return entry.cfg;
    }
    this.readConfig(pid, key);
    return CONFIG_PENDING;
  }

  // A re-read for a changed extent: now, or once CONFIG_FOLLOW_MS passed since the last one
  // (then the decision is made again, and asks with the extent of that moment).
  followConfig(pid, entry) {
    if (entry.reading || entry.timer) return;
    const wait = entry.at + CONFIG_FOLLOW_MS - Date.now();
    if (wait <= 0) return this.readConfig(pid);
    entry.timer = setTimeout(() => {
      entry.timer = null;
      this.apply();
    }, wait);
    if (entry.timer.unref) entry.timer.unref();
  }

  // A split reason for `pid`'s window: when the placement changed after the settings were
  // read (a Ghostty config reload changes the extent), read them again, at most every
  // CONFIG_RECHECK_MS. Returns whether a read runs (the old settings stay until it lands).
  configSuspect(pid, updatedUnix) {
    const entry = this.configs.get(pid);
    if (!entry || !entry.cfg) return false;
    if (entry.reading) return true;
    const now = Date.now();
    if (entry.at >= updatedUnix * 1000 || now - entry.at < CONFIG_RECHECK_MS) return false;
    this.readConfig(pid);
    return entry.reading;
  }

  // Read `pid`'s settings for the extent last asked about (`key`, else the entry's): one read
  // at a time, whichever asked (a changed extent, a split reason).
  readConfig(pid, key = null) {
    let entry = this.configs.get(pid);
    if (!entry) {
      entry = { cfg: null, at: 0, reading: false, key: null, seen: null, timer: null };
      this.configs.set(pid, entry);
    }
    if (key !== null) entry.seen = key;
    if (entry.reading) return;
    entry.reading = true;
    entry.key = entry.seen;
    this.readSettings(pid, !!this.probe.fakeFile, (cfg) => {
      entry.reading = false;
      if (this.stopped || this.configs.get(pid) !== entry) return;
      entry.cfg = cfg;
      entry.at = Date.now();
      this.apply();
    });
  }

  apply() {
    if (this.stopped) return;
    const now = Date.now();
    const d = this.core.decide(now);
    this.last = d;
    if (d.docked && !this.docked) this.enter();
    if (!d.docked && this.docked) this.leave();
    this.probe.watch(d.docked ? d.watch : {});
    for (const r of d.reports) this.feed.report(r.id, r.state, r.reason);
    // Settling: a decision once the window stopped moving, even without a sample.
    clearTimeout(this.settleTimer);
    this.settleTimer = null;
    if (typeof d.recheckAt === 'number') {
      this.settleTimer = setTimeout(() => {
        this.settleTimer = null;
        this.apply();
      }, Math.max(1, d.recheckAt - now));
      if (this.settleTimer.unref) this.settleTimer.unref();
    }
    if (!this.docked) return;
    if (d.tabId) this.host.dockShowsTab(d.tabId, d.target && d.target.placement);
    if (d.pair && this.orderFront) {
      this.orderFront(d.pair, (status, why) => {
        if (status !== 'ok') this.log(`dock: the terminal's window did not come along (${status}${why ? `: ${why}` : ''})`);
        this.core.pairDone(status);
        if (!this.stopped) this.apply();
      });
    }
    const win = this.host.dockWindow();
    if (!win) return;
    if (d.show && d.rect) {
      const b = win.getBounds();
      const r = d.rect;
      if (b.x !== r.x || b.y !== r.y || b.width !== r.width || b.height !== r.height) {
        win.setBounds({ x: r.x, y: r.y, width: r.width, height: r.height });
        this.host.layout();
      }
      if (d.fullScreen !== this.fullScreen && !this.headless) {
        this.fullScreen = d.fullScreen;
        win.setVisibleOnAllWorkspaces(d.fullScreen, { visibleOnFullScreen: d.fullScreen, skipTransformProcessType: true });
      }
      const dropped = this.setLevel(win, d.level) && d.level === 'normal';
      const first = !this.shown;
      if (first) {
        this.shown = true;
        if (!this.headless) win.showInactive();
        this.noteDockWid(win);
      }
      // back at the normal level: at its front, so directly above the terminal again at once
      if (d.level === 'normal' && (first || dropped || d.raise)) this.raise(win);
    } else if (this.shown) {
      this.shown = false;
      if (!this.headless) win.hide();
    }
  }

  // The dock window's level ('raised' or 'normal'); whether it changed. A level change can
  // reset the window's Spaces behaviour: a full-screen dock gets it again.
  setLevel(win, level) {
    if (level === this.level) return false;
    this.level = level;
    if (this.headless) return true;
    try {
      if (level === 'raised') win.setAlwaysOnTop(true, ...RAISED);
      else win.setAlwaysOnTop(false);
      if (this.fullScreen) win.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true, skipTransformProcessType: true });
    } catch (e) {
      this.log('dock: level change failed', e.message);
    }
    return true;
  }

  // Directly above the terminal window (normal level only: raised, nothing can cover it).
  raise(win) {
    if (this.headless || !this.core.bound || this.level === 'raised') return;
    try {
      win.moveAbove(`window:${this.core.bound.wid}:0`);
    } catch (e) {
      this.log('dock: moveAbove failed', e.message);
    }
    this.noteDockWid(win);
  }

  // Our dock window's number, so the core tells it from other windows above the terminal.
  noteDockWid(win) {
    if (this.headless || this.core.dockWid !== null) return;
    try {
      const id = String(win.getMediaSourceId() || '').split(':')[1];
      if (id) this.core.dockWid = Number(id);
    } catch {}
  }

  // The dock's window was clicked away from (the terminal ordered front): decided again, and
  // at the normal level back above the terminal at once.
  onBlur() {
    const win = this.host.dockWindow();
    if (!win || !this.shown) return;
    this.apply();
    if (this.shown && this.level === 'normal') this.raise(win);
  }

  enter() {
    this.docked = true;
    clearTimeout(this.policyTimer);
    this.log('dock: docked mode');
    // Out of ⌘-Tab and the Dock while part of herdr's window; needed to join a full-screen Space.
    if (!this.headless && process.platform === 'darwin' && this.electron.app.dock && !this.accessory) {
      this.accessory = true;
      this.electron.app.dock.hide();
    }
    // The frontmost app until the first activation notice.
    if (!this.headless && process.platform === 'darwin') {
      this.readFront((pid) => {
        if (pid && !this.core.front && !this.stopped) this.onFront(pid);
      });
    }
    this.host.enterDocked();
  }

  leave() {
    this.docked = false;
    this.shown = false;
    this.core.bound = null;
    this.core.dockWid = null;
    this.log('dock: window mode');
    const win = this.host.dockWindow();
    if (win) this.setLevel(win, 'normal');
    if (win && !this.headless) win.hide();
    this.host.leaveDocked();
    clearTimeout(this.policyTimer);
    this.policyTimer = setTimeout(() => {
      if (this.docked || !this.accessory) return;
      this.accessory = false;
      if (this.electron.app.dock) this.electron.app.dock.show().catch(() => {});
    }, POLICY_GRACE_MS);
  }

  // The test hook's view: inputs, the computed rect and the window's bounds.
  debug() {
    const win = this.host.dockWindow();
    return {
      docked: this.docked,
      shown: this.shown,
      decision: this.last,
      bound: this.core.bound,
      frame: this.core.frame,
      bounds: win ? win.getBounds() : null,
      visible: win ? win.isVisible() : false,
      probe_pid: this.probe.pid,
      accessory: this.accessory,
      level: this.level,
      front: this.core.front,
      paired: { for: this.core.pairedFor, at: this.core.pairAt, off: this.core.pairOff, missed: this.core.pairMissed },
    };
  }

  stop() {
    this.stopped = true;
    clearTimeout(this.settleTimer);
    clearTimeout(this.policyTimer);
    for (const entry of this.configs.values()) clearTimeout(entry.timer);
    if (this.frontSub !== null) {
      try {
        this.electron.systemPreferences.unsubscribeWorkspaceNotification(this.frontSub);
      } catch {}
      this.frontSub = null;
    }
    this.probe.stop();
  }
}

module.exports = { DockCore, DockController, SETTLE_MS, RESETTLE_MS, FRONT_SETTLE_MS, PAIR_MS, NO_WINDOW, NOT_MAC, CONFIG_PENDING, CONFIG_FOLLOW_MS, FRONT_TIMEOUT_MS, geometryKey, activatedPid, pairStatus };
