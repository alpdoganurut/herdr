// herdr's placements: the long poll (`browser.placements`), the reports
// (`browser.placement_report`) and which placement the dock follows (launcher design §3.3).
//
// Every herdr client that keeps an area for the browser publishes a placement: the herdr
// tab, the cells, its terminal's pixel geometry and its process ancestry. herdr adds its
// facts about the client (focused, foreground). The companion covers one: the target.

'use strict';

const { EventEmitter } = require('node:events');

const WAIT_MS = 20000; // herdr caps a wait at 25 s
const RETRY_MS = 1000;
const DOCK_KINDS = new Set(['side', 'full']);

// The placement the dock covers: among shown side/full placements, the foreground
// client's, else a focused one, else the newest. `null` when none is shown.
function chooseTarget(placements) {
  const shown = (placements || []).filter((p) => p && p.placement && p.placement.shown && DOCK_KINDS.has(p.placement.kind));
  if (!shown.length) return null;
  const newest = (list) => list.reduce((a, b) => ((b.updated_unix || 0) > (a.updated_unix || 0) ? b : a));
  const fg = shown.filter((p) => p.foreground);
  if (fg.length) return newest(fg);
  const focused = shown.filter((p) => p.focused);
  if (focused.length) return newest(focused);
  return newest(shown);
}

// Whether any client keeps an area for the browser (shown or not): the companion stays in
// docked mode across a herdr tab switch that hides it for a moment.
function anyDockPlacement(placements) {
  return (placements || []).some((p) => p && p.placement && DOCK_KINDS.has(p.placement.kind));
}

class PlacementFeed extends EventEmitter {
  // opts: {link (HerdrLink), log}
  constructor({ link, log = () => {} }) {
    super();
    this.link = link;
    this.log = log;
    this.running = false;
    this.bootId = null;
    this.rev = null;
    this.placements = [];
    this.reports = new Map(); // id -> "state|reason" last sent
    this.unsupported = false; // herdr without browser.placements
    this.timer = null;
  }

  start() {
    if (this.running) return;
    this.running = true;
    this._loop();
  }

  stop() {
    this.running = false;
    clearTimeout(this.timer);
  }

  // A new lease (a server restart or handoff): start over.
  reset() {
    this.bootId = null;
    this.rev = null;
    this.reports.clear();
    this.unsupported = false;
    if (this.running && !this.polling) this._loop();
  }

  _later(ms) {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this._loop(), ms);
  }

  async _loop() {
    if (!this.running || this.polling) return;
    if (!this.link.lease) return this._later(RETRY_MS);
    const features = this.link.lease.server_features || [];
    if (!features.includes('placements')) {
      if (!this.unsupported) this.log('placements: herdr does not publish them (no docking)');
      this.unsupported = true;
      return this._later(5000);
    }
    this.polling = true;
    let res;
    try {
      const params = { wait_ms: WAIT_MS };
      if (this.rev !== null) params.since_rev = this.rev;
      res = await this.link.call('browser.placements', params, { timeoutMs: WAIT_MS + 10000 });
    } catch (e) {
      this.polling = false;
      if (e.code === 'unknown_method') {
        this.unsupported = true;
        return this._later(30000);
      }
      return this._later(RETRY_MS);
    }
    this.polling = false;
    if (res.boot_id !== this.bootId) {
      this.bootId = res.boot_id;
      this.reports.clear();
    }
    this.rev = res.rev;
    this.placements = res.placements || [];
    this.emit('placements', this.placements);
    if (this.running) setImmediate(() => this._loop());
  }

  // Tell herdr what the dock does with placement `id` (only when it changed).
  async report(id, state, reason) {
    const key = `${state}|${reason || ''}`;
    if (this.reports.get(id) === key || !this.link.lease) return;
    this.reports.set(id, key);
    try {
      await this.link.call('browser.placement_report', { id, state, ...(reason ? { reason } : {}) });
    } catch (e) {
      this.reports.delete(id);
      this.log('placement_report failed', e.code || e.message);
    }
  }
}

module.exports = { PlacementFeed, chooseTarget, anyDockPlacement, DOCK_KINDS };
