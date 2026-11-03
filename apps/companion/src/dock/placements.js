// herdr's placements: the long poll (`browser.placements`), the reports
// (`browser.placement_report`) and which placement the dock follows (launcher design §3.3).
//
// Every herdr client that keeps an area for the browser publishes a placement: the herdr
// tab, the cells, its terminal's pixel geometry and its process ancestry. herdr adds its
// facts about the client (focused, foreground). The companion covers one: the target.
//
// A report is sent when it changes. herdr drops its report with the placement (a client
// that undocks), so the companion forgets what it sent about a placement that left the
// board, and about every placement when the board moved on by more than one change between
// two answers (it may have left and come back unseen): a placement that comes back is
// reported anew. Every request has its own connection (served on its own thread), so two
// reports in flight could land in either order: one goes at a time per placement, and a
// change meanwhile waits for it (only the latest goes).

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
    // id -> {sent: "state|reason" last sent (null: forgotten), inflight, next: the latest
    // change while one is in flight}
    this.reports = new Map();
    this.unsupported = false; // herdr without browser.placements
    this.timer = null;
    this.polling = false;
    // Bumped by stop() and reset(): an answer to a poll from before them is dropped.
    this.gen = 0;
  }

  start() {
    if (this.running) return;
    this.running = true;
    this._loop();
  }

  stop() {
    this.running = false;
    this.gen += 1;
    clearTimeout(this.timer);
    for (const r of this.reports.values()) r.next = null; // nothing more goes out
  }

  // A new lease (a server restart or handoff): start over.
  reset() {
    this.gen += 1;
    this.bootId = null;
    this.rev = null;
    this._forgetAll();
    this.unsupported = false;
    if (this.running && !this.polling) this._loop();
  }

  // What was sent about `id` no longer holds (herdr dropped it): the next report goes
  // again. One in flight stays tracked, so a newer one never overtakes it.
  _forget(id) {
    const r = this.reports.get(id);
    if (!r) return;
    if (!r.inflight) {
      this.reports.delete(id);
      return;
    }
    r.sent = null;
    r.next = null;
  }

  _forgetAll() {
    for (const id of [...this.reports.keys()]) this._forget(id);
  }

  _later(ms) {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this._loop(), ms);
  }

  _again() {
    if (this.running) setImmediate(() => this._loop());
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
    const gen = this.gen;
    let res;
    try {
      const params = { wait_ms: WAIT_MS };
      if (this.rev !== null) params.since_rev = this.rev;
      res = await this.link.call('browser.placements', params, { timeoutMs: WAIT_MS + 10000 });
    } catch (e) {
      this.polling = false;
      if (gen !== this.gen) return this._again(); // asked before a stop or a new lease
      if (e.code === 'unknown_method') {
        this.unsupported = true;
        return this._later(30000);
      }
      return this._later(RETRY_MS);
    }
    this.polling = false;
    // An answer to a poll from before a stop or a new lease: nothing of it is kept.
    if (gen !== this.gen) return this._again();
    if (res.boot_id !== this.bootId) {
      this.bootId = res.boot_id;
      this._forgetAll();
    } else if (this.rev !== null && res.rev > this.rev + 1) {
      this._forgetAll(); // changes between two answers were missed
    }
    this.rev = res.rev;
    this.placements = res.placements || [];
    const ids = new Set(this.placements.map((p) => p && p.placement && p.placement.id).filter(Boolean));
    for (const id of [...this.reports.keys()]) if (!ids.has(id)) this._forget(id);
    this.emit('placements', this.placements);
    this._again();
  }

  // Tell herdr what the dock does with placement `id` (only when it changed). While a
  // report on it is in flight the change waits; a change back to what is in flight
  // cancels the wait.
  report(id, state, reason) {
    if (!this.link.lease) return;
    const key = `${state}|${reason || ''}`;
    let r = this.reports.get(id);
    if (!r) {
      r = { sent: null, inflight: false, next: null };
      this.reports.set(id, r);
    }
    if (r.inflight) {
      r.next = key === r.sent ? null : { state, reason, key };
      return;
    }
    if (r.sent === key) return;
    this._send(id, r, state, reason, key);
  }

  async _send(id, r, state, reason, key) {
    r.inflight = true;
    r.sent = key;
    r.next = null;
    try {
      await this.link.call('browser.placement_report', { id, state, ...(reason ? { reason } : {}) });
    } catch (e) {
      if (r.sent === key) r.sent = null; // the next report tries again
      this.log('placement_report failed', e.code || e.message);
    }
    r.inflight = false;
    const next = r.next;
    r.next = null;
    if (next && next.key !== r.sent && this.link.lease) return this._send(id, r, next.state, next.reason, next.key);
    if (r.sent === null && this.reports.get(id) === r) this.reports.delete(id);
  }
}

module.exports = { PlacementFeed, chooseTarget, anyDockPlacement, DOCK_KINDS };
