// Mock herdr API socket server for companion development (contract §1/§2/§3/§5 shapes).
// It plays both the herdr server (JSON lines over a Unix socket) and, optionally, the
// sidecar's attach check (connectOverCDP + control-page nonce verification).
//
// Usage: node test/mock-herdr.mjs --socket /tmp/x.sock [--verify-cdp]
// Test-only methods (not part of herdr): mock.dump, mock.focus_tab, mock.create_tab,
// mock.close_tab, mock.rename_tab, mock.restart (drops every connection and lease,
// keeps tab state, like a live handoff), mock.control (evaluates a control-page call
// over CDP like the sidecar would), mock.shutdown.
import net from 'node:net';
import fs from 'node:fs';
import crypto from 'node:crypto';
import http from 'node:http';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

export function createMockHerdr({ socketPath, verifyCdp = false, log = () => {}, heartbeatMs = 5000, ttlMs = 15000 } = {}) {
  const state = {
    focused: 'w1:t1',
    tabs: new Map([
      ['w1:t1', { tab_id: 'w1:t1', workspace_id: 'w1', label: 'planner', browser: null }],
      ['w1:t2', { tab_id: 'w1:t2', workspace_id: 'w1', label: 'builder', browser: null }],
    ]),
    nextTab: 3,
    nextEntry: 1,
    lease: null, // {lease_id, instance, expires, seq, cdp, nonce, version}
    syncs: [],
    reports: [],
    sent: [],
    registers: [],
    sockets: new Set(),
    subscribers: new Set(),
    browser: null, // playwright Browser when verifyCdp
  };
  const mintKey = () => 'g-' + crypto.randomBytes(8).toString('hex');
  const now = () => Math.floor(Date.now() / 1000);

  const err = (id, code, message) => ({ id, error: { code, message } });
  const ok = (id, result) => ({ id, result });
  const leaseOk = (params) => state.lease && params && params.lease_id === state.lease.lease_id && state.lease.expires >= Date.now();

  function emit(event, data) {
    for (const s of state.subscribers) {
      if (s.kinds.has(event)) s.sock.write(JSON.stringify({ event, data }) + '\n');
    }
  }

  function hostState(tabIds) {
    const tabs = [];
    for (const t of state.tabs.values()) {
      if (tabIds && !tabIds.includes(t.tab_id)) continue;
      const out = { tab_id: t.tab_id, workspace_id: t.workspace_id, label: t.label };
      if (t.browser) {
        out.key = t.browser.key; out.rev = t.browser.rev; out.active = t.browser.active;
        out.entries = t.browser.entries.map(({ target, view_id, ...e }) => e);
      }
      tabs.push(out);
    }
    return { focused_tab_id: state.focused, tabs };
  }

  async function probe(port) {
    return new Promise((resolve) => {
      const req = http.get({ host: '127.0.0.1', port, path: '/json/version', timeout: 2000 }, (res) => {
        let body = ''; res.on('data', (d) => (body += d)); res.on('end', () => { try { resolve(JSON.parse(body)); } catch { resolve(null); } });
      });
      req.on('error', () => resolve(null)); req.on('timeout', () => { req.destroy(); resolve(null); });
    });
  }

  async function controlPage() {
    if (!state.browser) return null;
    for (const ctx of state.browser.contexts()) {
      for (const p of ctx.pages()) if (p.url().startsWith('herdr-companion://control/')) return p;
    }
    return null;
  }

  async function verify(cdp, nonce) {
    const { chromium } = require('playwright-core');
    if (state.browser) { await state.browser.close().catch(() => {}); state.browser = null; }
    state.browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdp.port}`, { timeout: 15000 });
    let page = null;
    for (let i = 0; i < 50 && !page; i++) { page = await controlPage(); if (!page) await new Promise((r) => setTimeout(r, 100)); }
    if (!page) throw Object.assign(new Error('control page not found'), { code: 'host_verify_failed' });
    const got = await page.evaluate(() => window.__herdrCompanion.nonce());
    if (got !== nonce) throw Object.assign(new Error('nonce mismatch'), { code: 'host_verify_failed' });
  }

  async function handle(req, sock) {
    const { id, method, params = {} } = req;
    switch (method) {
      case 'browser.host_register': {
        state.registers.push({ at: Date.now(), instance: params.instance });
        if (params.kind !== 'companion' || params.protocol !== 1) return err(id, 'host_protocol_unsupported', 'protocol 1 only');
        if (!params.cdp || params.cdp.host !== '127.0.0.1') return err(id, 'invalid_request', 'cdp must be loopback');
        if (state.lease && state.lease.expires >= Date.now() && state.lease.instance !== params.instance) {
          return err(id, 'host_in_use', `companion ${state.lease.instance} ${state.lease.version} holds the lease`);
        }
        const v = await probe(params.cdp.port);
        if (!v) return err(id, 'host_unreachable', `no DevTools at 127.0.0.1:${params.cdp.port}`);
        if (verifyCdp) {
          try { await verify(params.cdp, params.nonce); } catch (e) { return err(id, e.code || 'attach_blocked', e.message); }
        }
        state.lease = { lease_id: 'L' + crypto.randomBytes(6).toString('hex'), instance: params.instance, expires: Date.now() + ttlMs, seq: 0, cdp: params.cdp, nonce: params.nonce, version: params.version };
        return ok(id, { type: 'browser_host_lease', lease: leaseBody() });
      }
      case 'browser.host_heartbeat':
        if (!leaseOk(params)) return err(id, 'lease_unknown', 'no such lease');
        state.lease.expires = Date.now() + ttlMs;
        return ok(id, { type: 'browser_host_lease', lease: leaseBody() });
      case 'browser.host_unregister':
        if (!leaseOk(params)) return err(id, 'lease_unknown', 'no such lease');
        state.lease = null;
        return ok(id, { type: 'ok' });
      case 'browser.host_state':
        if (!leaseOk(params)) return err(id, 'lease_unknown', 'no such lease');
        return ok(id, { type: 'browser_host_state', state: hostState(params.tab_ids) });
      case 'browser.host_sync': {
        if (!leaseOk(params)) return err(id, 'lease_unknown', 'no such lease');
        if (JSON.stringify(params).length > 256 * 1024) return err(id, 'too_large', 'sync over 256 KiB');
        if (!(params.seq > state.lease.seq)) return err(id, 'stale_seq', `seq ${params.seq} <= ${state.lease.seq}`);
        state.lease.seq = params.seq;
        state.syncs.push(params);
        const keys = {}; const unknown = [];
        for (const g of params.groups || []) {
          const t = state.tabs.get(g.tab_id);
          if (!t) { unknown.push(g.tab_id); continue; }
          const key = g.key || (t.browser && t.browser.key) || mintKey();
          keys[g.tab_id] = key;
          const entries = (g.entries || []).slice(0, 32).map((e) => {
            const out = { ...e };
            if (!out.id) { out.id = 'b' + (state.nextEntry++).toString(32); if (e.view_id) keys[`${g.tab_id}/${e.view_id}`] = out.id; }
            return out;
          });
          let active = g.active;
          if (active && !entries.some((e) => e.id === active)) {
            const byView = entries.find((e) => e.view_id === active);
            active = byView ? byView.id : null;
          }
          t.browser = { key, entries, active: active || null, rev: ((t.browser && t.browser.rev) || 0) + 1 };
        }
        return ok(id, { type: 'browser_host_synced', keys, unknown_tabs: unknown });
      }
      case 'browser.host_report': {
        if (!leaseOk(params)) return err(id, 'lease_unknown', 'no such lease');
        const events = params.events || [];
        const accepted = events.slice(0, 50);
        state.reports.push(...accepted);
        return ok(id, { type: 'browser_host_reported', accepted: accepted.length, dropped: events.length - accepted.length });
      }
      case 'browser.send_page': {
        if (!leaseOk(params)) return err(id, 'lease_unknown', 'no such lease');
        const t = state.tabs.get(params.tab_id);
        if (!t) return err(id, 'tab_not_found', `tab ${params.tab_id} not found`);
        if (t.no_agent) return err(id, 'no_agent_in_tab', 'no agent in this tab');
        state.sent.push(params);
        return ok(id, { type: 'browser_send_page', outcome: 'sent', to_pane: params.tab_id.split(':')[0] + ':p1', to_name: `${t.label} · claude`, message_id: 'm' + state.sent.length });
      }
      case 'tab.focus': {
        const t = state.tabs.get(params.tab_id);
        if (!t) return err(id, 'tab_not_found', 'no tab');
        focus(params.tab_id);
        return ok(id, { type: 'ok' });
      }
      case 'events.subscribe': {
        const kinds = new Set((params.subscriptions || []).map((s) => s.type));
        state.subscribers.add({ sock, kinds });
        sock.on('close', () => { for (const s of state.subscribers) if (s.sock === sock) state.subscribers.delete(s); });
        return ok(id, { type: 'subscription_started' });
      }
      // ---- test-only ----
      case 'mock.dump':
        return ok(id, { type: 'mock_dump', focused: state.focused, lease: state.lease && { ...state.lease }, tabs: [...state.tabs.values()], syncs: state.syncs.length, last_sync: state.syncs.at(-1) || null, reports: state.reports, sent: state.sent, registers: state.registers });
      case 'mock.focus_tab': focus(params.tab_id); return ok(id, { type: 'ok' });
      case 'mock.create_tab': {
        const tab_id = `${params.workspace_id || 'w1'}:t${state.nextTab++}`;
        const t = { tab_id, workspace_id: params.workspace_id || 'w1', label: params.label || 'new', browser: null };
        state.tabs.set(tab_id, t);
        emit('tab.created', { type: 'tab_created', tab: { tab_id, workspace_id: t.workspace_id, number: state.tabs.size, label: t.label, focused: false, pane_count: 1, agent_status: 'unknown' } });
        return ok(id, { type: 'ok', tab_id });
      }
      case 'mock.close_tab': {
        const t = state.tabs.get(params.tab_id);
        state.tabs.delete(params.tab_id);
        if (t) emit('tab.closed', { type: 'tab_closed', tab_id: t.tab_id, workspace_id: t.workspace_id });
        return ok(id, { type: 'ok' });
      }
      case 'mock.rename_tab': {
        const t = state.tabs.get(params.tab_id); if (t) t.label = params.label;
        emit('tab.renamed', { type: 'tab_renamed', tab_id: params.tab_id, workspace_id: t ? t.workspace_id : 'w1', label: params.label });
        return ok(id, { type: 'ok' });
      }
      case 'mock.restart':
        setTimeout(() => restart(), 10);
        return ok(id, { type: 'ok' });
      case 'mock.control': {
        const page = await controlPage();
        if (!page) return err(id, 'host_unavailable', 'no control page');
        const value = await page.evaluate(params.expression);
        return ok(id, { type: 'mock_control', value });
      }
      case 'mock.shutdown':
        setTimeout(() => close(), 10);
        return ok(id, { type: 'ok' });
      default:
        return err(id, 'unknown_method', `unknown method ${method}`);
    }
  }

  function leaseBody() {
    return { lease_id: state.lease.lease_id, profile: 'companion', state: 'attached', heartbeat_ms: heartbeatMs, ttl_ms: ttlMs, expires_unix: now() + Math.round(ttlMs / 1000), host_protocol: 1, server_features: ['host_sync', 'host_report', 'send_page', 'show', 'page_errors'] };
  }

  function focus(tab_id) {
    const t = state.tabs.get(tab_id); if (!t) return;
    const prevWs = state.tabs.get(state.focused)?.workspace_id;
    state.focused = tab_id;
    if (prevWs !== t.workspace_id) emit('workspace.focused', { type: 'workspace_focused', workspace_id: t.workspace_id });
    emit('tab.focused', { type: 'tab_focused', tab_id, workspace_id: t.workspace_id });
  }

  let server;
  function listen() {
    try { fs.unlinkSync(socketPath); } catch {}
    server = net.createServer((sock) => {
      state.sockets.add(sock);
      sock.on('close', () => state.sockets.delete(sock));
      sock.on('error', () => {});
      // Like herdr's API server: one request per connection, closed after the answer
      // (events.subscribe keeps it open as the stream).
      let buf = '';
      let taken = false;
      sock.on('data', (d) => {
        if (taken) return;
        buf += d;
        const i = buf.indexOf('\n');
        if (i < 0) return;
        const line = buf.slice(0, i);
        if (!line.trim()) return sock.end();
        taken = true;
        let req;
        try { req = JSON.parse(line); } catch { sock.end(JSON.stringify(err('', 'invalid_json', 'bad json')) + '\n'); return; }
        log('<-', req.method, JSON.stringify(req.params || {}).slice(0, 160));
        const keep = req.method === 'events.subscribe';
        const send = (res) => { if (sock.destroyed) return; const out = JSON.stringify(res) + '\n'; if (keep) sock.write(out); else sock.end(out); };
        Promise.resolve(handle(req, sock)).then(send).catch((e) => send(err(req.id, 'internal', String(e))));
      });
    });
    return new Promise((r) => server.listen(socketPath, r));
  }

  async function restart() {
    // Live handoff: connections drop, leases vanish (new server), tab state survives with targets cleared.
    state.lease = null;
    for (const t of state.tabs.values()) if (t.browser) t.browser.entries = t.browser.entries.map(({ target, view_id, ...e }) => e);
    for (const s of state.sockets) s.destroy();
    state.subscribers.clear();
    await new Promise((r) => server.close(r));
    if (state.browser) { await state.browser.close().catch(() => {}); state.browser = null; }
    await new Promise((r) => setTimeout(r, 300));
    await listen();
    log('restarted');
  }

  async function close() {
    for (const s of state.sockets) s.destroy();
    if (state.browser) await state.browser.close().catch(() => {});
    await new Promise((r) => server.close(r));
    try { fs.unlinkSync(socketPath); } catch {}
  }

  return { listen, close, restart, state, emit, focus };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const args = process.argv.slice(2);
  const socketPath = args[args.indexOf('--socket') + 1] || '/tmp/herdr-companion-mock.sock';
  const mock = createMockHerdr({ socketPath, verifyCdp: args.includes('--verify-cdp'), log: (...a) => console.log(new Date().toISOString().slice(11, 23), ...a) });
  mock.listen().then(() => console.log('mock herdr listening on', socketPath));
}
