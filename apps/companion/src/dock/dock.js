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
//   hidden      not now (the placement is not shown, or the window is off screen)
//   moving      the window moves or resizes: hidden until it settles (120 ms)
//   unbound     several terminal windows could be this client's: one click binds it
//   unsupported with a reason the user can act on
//   elsewhere   another client's placement is the target
// "Covered" is handled by z-order: the dock sits directly above the terminal window, so
// whatever covers the terminal covers the dock the same way.

'use strict';

const { execFile } = require('node:child_process');
const { placementRect, isFullScreen, REASONS } = require('./geometry.js');
const { chooseTarget, anyDockPlacement } = require('./placements.js');
const { readGhosttyConfig } = require('./ghostty-config.js');

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
function readTerminalSettings(pid, fake, done) {
  execFile('/bin/ps', ['-o', 'args=', '-p', String(pid)], { encoding: 'utf8', timeout: 2000 }, (err, stdout) => {
    const args = err ? '' : String(stdout || '').trim();
    const ghostty = fake || /ghostty/i.test(args.split(/\s+/)[0] || '');
    done(ghostty ? readGhosttyConfig({ argv: args.split(/\s+/).slice(1) }) : { symmetric: true, sources: [] });
  });
}

const NO_WINDOW = 'Docking needs herdr directly in a terminal window (not inside tmux or ssh)';
const NOT_MAC = 'Docking is macOS only for now';

const sameFrame = (a, b) => !!a && !!b && a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;

class DockCore {
  // env: {platform, scaleFor(frame) -> number, displayFor(frame) -> {x,y,width,height},
  //       configFor(pid, key) -> ghostty config | CONFIG_PENDING (`key`: the placement's
  //         extent and cell size, geometryKey),
  //       configSuspect?(pid, updatedUnix) -> whether the settings are being read again}
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
  //   {docked, show, rect, fullScreen, raise, target, tabId, watch: {pids, wid},
  //    reports: [{id, state, reason}], recheckAt}
  // `recheckAt`: while the dock waits for a window to settle, when the decision changes
  // without a new sample or placement (null otherwise).
  decide(now) {
    const out = { docked: anyDockPlacement(this.placements), show: false, rect: null, fullScreen: false, raise: false, target: null, tabId: null, watch: { pids: [], wid: 0 }, reports: [], recheckAt: null };
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
    const t = this.sample && this.sample.target;
    if (t && t.num === this.bound.wid && t.on === false) return say('hidden');
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
    out.raise = this.dockWid === null || last !== this.dockWid;
    return say('covering');
  }
}

// The Electron side. `host` is the companion (main.js): views, windows, strip, groups.
class DockController {
  // `readSettings(pid, fake, done)`: the terminal's settings (tests replace it).
  constructor({ host, feed, probe, log = () => {}, headless = false, platform = process.platform, electron, readSettings = readTerminalSettings }) {
    this.host = host;
    this.feed = feed;
    this.probe = probe;
    this.log = log;
    this.headless = headless;
    this.electron = electron;
    this.readSettings = readSettings;
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
    });
    this.docked = false;
    this.shown = false;
    this.stopped = false;
    this.last = null; // the last decision
    this.fullScreen = false;
    this.policyTimer = null;
    this.settleTimer = null;
    this.accessory = false;
    feed.on('placements', (list) => {
      this.core.setPlacements(list);
      this.apply();
    });
    probe.on('sample', (s) => {
      this.core.onSample(s, Date.now());
      this.apply();
    });
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
    if (d.tabId) this.host.dockShowsTab(d.tabId);
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
      if (!this.shown) {
        this.shown = true;
        if (!this.headless) win.showInactive();
        this.raise(win);
      } else if (d.raise) this.raise(win);
    } else if (this.shown) {
      this.shown = false;
      if (!this.headless) win.hide();
    }
  }

  raise(win) {
    if (this.headless || !this.core.bound) return;
    try {
      win.moveAbove(`window:${this.core.bound.wid}:0`);
    } catch (e) {
      this.log('dock: moveAbove failed', e.message);
    }
    if (this.core.dockWid === null) {
      const id = String(win.getMediaSourceId() || '').split(':')[1];
      if (id) this.core.dockWid = Number(id);
    }
  }

  // The dock's window was clicked away from (the terminal ordered front): back above it.
  onBlur() {
    const win = this.host.dockWindow();
    if (win && this.shown) this.raise(win);
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
    this.host.enterDocked();
  }

  leave() {
    this.docked = false;
    this.shown = false;
    this.core.bound = null;
    this.core.dockWid = null;
    this.log('dock: window mode');
    const win = this.host.dockWindow();
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
    };
  }

  stop() {
    this.stopped = true;
    clearTimeout(this.settleTimer);
    clearTimeout(this.policyTimer);
    for (const entry of this.configs.values()) clearTimeout(entry.timer);
    this.probe.stop();
  }
}

module.exports = { DockCore, DockController, SETTLE_MS, RESETTLE_MS, NO_WINDOW, NOT_MAC, CONFIG_PENDING, CONFIG_FOLLOW_MS, geometryKey };
