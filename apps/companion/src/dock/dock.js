// The docked companion (launcher design §3.4): a pure core that decides, and an Electron
// adapter that does what it decided.
//
// herdr publishes placements (dock/placements.js); the window probe (dock/window-probe.js)
// says where the terminal windows are. The core binds the target placement to its terminal
// window (the client's process ancestry owns it; with several, the frontmost when the
// client's focus flips or the user clicks the "unbound" placeholder), follows it, and says
// when the dock window shows and where (dock/geometry.js), and what to report to herdr:
//   covering    over the area
//   hidden      not now (the placement is not shown, or the window is off screen)
//   moving      the window moves or resizes: hidden until it settles (120 ms)
//   unbound     several terminal windows could be this client's: one click binds it
//   unsupported with a reason the user can act on
//   elsewhere   another client's placement is the target
// "Covered" is handled by z-order: the dock sits directly above the terminal window, so
// whatever covers the terminal covers the dock the same way.

'use strict';

const { placementRect, isFullScreen, REASONS } = require('./geometry.js');
const { chooseTarget, anyDockPlacement } = require('./placements.js');

const SETTLE_MS = 120;
// Right after a move or resize herdr's placement may still describe the old size: a
// geometry that does not fit is "moving" this long before it is "unsupported".
const RESETTLE_MS = 1500;
// The Dock icon comes back this long after the last placement went.
const POLICY_GRACE_MS = 2000;

const NO_WINDOW = 'Docking needs herdr directly in a terminal window (not inside tmux or ssh)';
const NOT_MAC = 'Docking is macOS only for now';

const sameFrame = (a, b) => !!a && !!b && a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;

class DockCore {
  // env: {platform, scaleFor(frame) -> number, displayFor(frame) -> {x,y,width,height},
  //       configFor(pid) -> ghostty config}
  constructor(env) {
    this.env = env;
    this.placements = [];
    this.bound = null; // {wid, pid, placementId}
    this.seen = new Map(); // placement id -> {focused, nudge}
    this.rebind = new Set(); // placement ids whose focus flipped or nudge changed
    this.sample = null;
    this.candidates = null; // last candidate list (null: none seen yet)
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
      if ((p.focused && !was.focused) || nudge !== was.nudge) this.rebind.add(id);
      this.seen.set(id, { focused: !!p.focused, nudge });
    }
    for (const id of [...this.seen.keys()]) if (!ids.has(id)) this.seen.delete(id);
    if (this.bound && !ids.has(this.bound.placementId)) this.bound = null;
  }

  onSample(sample, now) {
    this.sample = sample;
    if (sample.candidates) this.candidates = sample.candidates;
    const t = sample.target;
    if (this.bound && t && t.num === this.bound.wid) {
      if (!sameFrame(t, this.frame)) {
        this.frame = { x: t.x, y: t.y, w: t.w, h: t.h };
        this.frameSince = now;
        this.movedAt = now;
      }
    }
  }

  // The geometry of `placement` over the window `win` (a probe window record).
  rectFor(placement, win) {
    const frame = { x: win.x, y: win.y, w: win.w, h: win.h };
    return placementRect({
      placement,
      frame,
      scale: this.env.scaleFor(frame),
      config: this.env.configFor(win.pid),
    });
  }

  // Bind the target to its window; returns {state, reason} when it cannot.
  _bind(target) {
    const placement = target.placement;
    const pids = placement.pids || [];
    if (this.bound && this.bound.placementId === placement.id) {
      const cand = (this.candidates || []).find((w) => w.num === this.bound.wid);
      const t = this.sample && this.sample.target;
      const live = cand || (t && t.num === this.bound.wid);
      // Keep the window until it leaves the list or another one is asked for.
      if (live && !this.rebind.has(placement.id)) return null;
    }
    if (this.candidates === null) return { state: 'hidden' }; // nothing seen yet
    const fits = this.candidates.filter((w) => pids.includes(w.pid) && this.rectFor(placement, w).ok);
    const mine = this.candidates.filter((w) => pids.includes(w.pid));
    if (!mine.length) {
      this.bound = null;
      return { state: 'unsupported', reason: NO_WINDOW };
    }
    if (!fits.length) {
      // the client's windows exist but none matches its grid: say why
      const why = this.rectFor(placement, mine[0]);
      this.bound = null;
      return { state: 'unsupported', reason: why.reason || REASONS.split };
    }
    let pick = null;
    if (fits.length === 1) pick = fits[0];
    else if (this.rebind.has(placement.id)) pick = fits[0]; // front to back: the frontmost
    this.rebind.delete(placement.id);
    if (!pick) {
      if (this.bound && fits.some((w) => w.num === this.bound.wid)) return null;
      this.bound = null;
      return { state: 'unbound' };
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
  //    reports: [{id, state, reason}]}
  decide(now) {
    const out = { docked: anyDockPlacement(this.placements), show: false, rect: null, fullScreen: false, raise: false, target: null, tabId: null, watch: { pids: [], wid: 0 }, reports: [] };
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
    if (this.frameSince && now - this.frameSince < SETTLE_MS) return say('moving');
    const win = { ...this.frame, pid: this.bound.pid };
    const geo = this.rectFor(placement, win);
    if (!geo.ok) {
      if (this.movedAt && now - this.movedAt < RESETTLE_MS) return say('moving');
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
  constructor({ host, feed, probe, log = () => {}, headless = false, platform = process.platform, electron }) {
    this.host = host;
    this.feed = feed;
    this.probe = probe;
    this.log = log;
    this.headless = headless;
    this.electron = electron;
    const { screen } = electron;
    this.configs = new Map(); // pid -> ghostty config
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
      configFor: (pid) => this.configFor(pid),
    });
    this.docked = false;
    this.shown = false;
    this.last = null; // the last decision
    this.fullScreen = false;
    this.policyTimer = null;
    this.accessory = false;
    feed.on('placements', (list) => {
      this.core.setPlacements(list);
      this.apply();
    });
    probe.on('sample', (s) => {
      this.core.onSample(s, Date.now());
      this.apply();
    });
    // Settling needs a decision after the frame stops changing even without a sample.
    this.tick = setInterval(() => this.docked && this.apply(), 60);
    this.tick.unref();
  }

  // The terminal's geometry settings: Ghostty's config files and its own flags (they win;
  // `ps`, no permission); another terminal is read as evenly padded (best effort).
  configFor(pid) {
    if (this.configs.has(pid)) return this.configs.get(pid);
    let args = '';
    try {
      args = require('node:child_process').execFileSync('/bin/ps', ['-o', 'args=', '-p', String(pid)], { encoding: 'utf8', timeout: 1000 }).trim();
    } catch {}
    let cfg;
    if (this.probe.fakeFile || /ghostty/i.test(args.split(/\s+/)[0] || '')) {
      cfg = require('./ghostty-config.js').readGhosttyConfig({ argv: args.split(/\s+/).slice(1) });
    } else cfg = { symmetric: true, sources: [] };
    this.configs.set(pid, cfg);
    return cfg;
  }

  apply() {
    const now = Date.now();
    const d = this.core.decide(now);
    this.last = d;
    if (d.docked && !this.docked) this.enter();
    if (!d.docked && this.docked) this.leave();
    this.probe.watch(d.docked ? d.watch : {});
    for (const r of d.reports) this.feed.report(r.id, r.state, r.reason);
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
    clearInterval(this.tick);
    clearTimeout(this.policyTimer);
    this.probe.stop();
  }
}

module.exports = { DockCore, DockController, SETTLE_MS, RESETTLE_MS, NO_WINDOW, NOT_MAC };
