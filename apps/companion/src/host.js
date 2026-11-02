// Browser-host role (contract §1.2/§1.3/§3): register, heartbeat lease, events stream,
// reconnect with backoff after a server restart / live handoff (same instance).
'use strict';
const { EventEmitter } = require('node:events');
const { Connection, ApiError } = require('./herdr-api.js');

const EVENT_KINDS = [
  'tab.focused',
  'workspace.focused',
  'tab.created',
  'tab.closed',
  'tab.renamed',
  'tab.moved',
  'workspace.closed',
  'workspace.renamed',
];
const BACKOFF_MS = [500, 1000, 2000, 4000, 5000];
const SLOW_RETRY_MS = 5000;

// Errors after which retrying cannot help until something on the herdr side changes.
const TERMINAL = new Set(['unknown_method', 'host_protocol_unsupported']);
const SLOW = new Set(['host_in_use', 'browser_disabled', 'host_disabled', 'host_verify_failed', 'attach_blocked']);

class HerdrLink extends EventEmitter {
  // opts: {socketPath, register: () => params, log}
  // herdr's API server answers ONE request per connection and then closes it, so every
  // request gets its own connection; only the events.subscribe stream stays open. The
  // stream closing (server restart, live handoff) or a failed heartbeat means "lost".
  constructor(opts) {
    super();
    this.opts = opts;
    this.log = opts.log || (() => {});
    this.events = null;
    this.lease = null;
    this.gen = 0; // bumped on every (re)connect and teardown: stale async results are ignored
    this.attempt = 0;
    this.stopped = false;
    this.hbTimer = null;
    this.retryTimer = null;
    this.buffer = null; // events queued until the app has applied host_state
    this.status = { state: 'connecting', text: 'connecting to herdr…' };
  }

  setStatus(state, text) {
    this.status = { state, text };
    this.emit('status', this.status);
  }

  start() {
    this.stopped = false;
    this._connect();
  }

  _schedule(ms) {
    if (this.stopped) return;
    clearTimeout(this.retryTimer);
    this.retryTimer = setTimeout(() => this._connect(), ms);
  }

  _backoff() {
    const ms = BACKOFF_MS[Math.min(this.attempt, BACKOFF_MS.length - 1)];
    this.attempt += 1;
    return ms;
  }

  // One request on a fresh connection. Connect failures reject with code 'unreachable'.
  async _rpc(method, params = {}, { timeoutMs = 15000 } = {}) {
    const conn = new Connection(this.opts.socketPath, { prefix: 'c', timeoutMs });
    conn.on('error', () => {});
    try {
      await conn.connect();
    } catch (e) {
      throw new ApiError('unreachable', `herdr not reachable (${e.code || e.message})`);
    }
    try {
      return await conn.request(method, params, { timeoutMs });
    } finally {
      conn.close();
    }
  }

  async _connect() {
    if (this.stopped) return;
    this._teardown();
    const gen = this.gen;
    let lease;
    try {
      // The server attaches its sidecar before answering: up to 2×ATTACH_TIMEOUT (30 s).
      const res = await this._rpc('browser.host_register', this.opts.register(), { timeoutMs: 70000 });
      lease = res.lease;
    } catch (e) {
      if (gen !== this.gen) return;
      if (TERMINAL.has(e.code)) {
        this.setStatus(
          'too_old',
          e.code === 'unknown_method' ? 'this herdr is too old for the companion' : `herdr refused: ${e.message}`,
        );
        this.stopped = true;
        return;
      }
      if (SLOW.has(e.code)) {
        this.setStatus('refused', `${e.code}: ${e.message}`);
        return this._schedule(SLOW_RETRY_MS);
      }
      this.setStatus('connecting', e.code === 'unreachable' ? `${e.message}; retrying` : `register failed (${e.code || e.message}); retrying`);
      return this._schedule(this._backoff());
    }
    if (gen !== this.gen || this.stopped) return;
    this.lease = lease;
    this.attempt = 0;
    // Events first, host_state after (in the app), buffering in between: no tab.focused is lost.
    this.buffer = [];
    const ev = new Connection(this.opts.socketPath, { prefix: 'e' });
    // Before connect: a failed connect emits `error`, and an unheard one throws.
    ev.on('error', () => {});
    try {
      await ev.connect();
      ev.on('event', (e) => this._onEvent(e));
      ev.on('close', () => gen === this.gen && this._lost('events stream closed'));
      this.events = ev;
      await ev.request('events.subscribe', { subscriptions: EVENT_KINDS.map((type) => ({ type })) }, { stream: true });
    } catch (e) {
      this.log('subscribe failed', e.message);
      ev.close();
      return this._lost('subscribe failed');
    }
    if (gen !== this.gen) return;
    this._heartbeat();
    this.setStatus('registered', `attached as ${lease.profile}`);
    this.emit('lease', lease);
  }

  _onEvent(e) {
    if (this.buffer) this.buffer.push(e);
    else this.emit('event', e);
  }

  // The app calls this once host_state is applied.
  releaseEvents() {
    const q = this.buffer || [];
    this.buffer = null;
    for (const e of q) this.emit('event', e);
  }

  _heartbeat() {
    clearInterval(this.hbTimer);
    const every = Math.max(500, (this.lease && this.lease.heartbeat_ms) || 5000);
    const gen = this.gen;
    this.hbTimer = setInterval(async () => {
      if (!this.lease || gen !== this.gen) return;
      try {
        const res = await this._rpc('browser.host_heartbeat', { lease_id: this.lease.lease_id }, { timeoutMs: every * 2 });
        if (gen === this.gen && res && res.lease) this.lease = { ...this.lease, ...res.lease };
      } catch (e) {
        if (gen !== this.gen) return;
        if (e.code === 'lease_unknown' || e.code === 'timeout' || e.code === 'unreachable') this._lost(`heartbeat: ${e.code}`);
      }
    }, every);
  }

  _lost(why) {
    if (!this.events && !this.lease) return;
    this.log('herdr link lost:', why);
    this._teardown();
    this.emit('lost', why);
    if (!this.stopped) {
      this.setStatus('connecting', `reconnecting to herdr (${why})`);
      this._schedule(this._backoff());
    }
  }

  _teardown() {
    this.gen += 1;
    clearInterval(this.hbTimer);
    this.hbTimer = null;
    const ev = this.events;
    this.events = null;
    this.lease = null;
    this.buffer = null;
    if (ev) ev.close();
  }

  // A request that needs a lease (lease_id added). Rejects with code 'no_lease' when detached.
  async call(method, params = {}) {
    if (!this.lease) {
      const e = new Error('not attached to herdr');
      e.code = 'no_lease';
      throw e;
    }
    const gen = this.gen;
    try {
      return await this._rpc(method, { lease_id: this.lease.lease_id, ...params });
    } catch (e) {
      if (e.code === 'lease_unknown' && gen === this.gen) this._lost('lease_unknown');
      throw e;
    }
  }

  // Plain request (no lease), e.g. tab.focus.
  async request(method, params = {}) {
    return this._rpc(method, params);
  }

  async stop() {
    this.stopped = true;
    clearTimeout(this.retryTimer);
    if (this.lease) {
      await Promise.race([
        this._rpc('browser.host_unregister', { lease_id: this.lease.lease_id }).catch(() => {}),
        new Promise((r) => setTimeout(r, 1000)),
      ]);
    }
    this._teardown();
  }
}

module.exports = { HerdrLink, EVENT_KINDS };
