// The companion's `--remote` mode (src/remote.js) against the mock herdr, an authenticating
// proxy that answers every page itself (it dials nothing) and a canary on this Mac's loopback
// that must never be reached. Hidden: one Electron per scenario, each with a hard timer
// (e2e-remote-main.cjs), a fresh profile, no clipboard, no download.
//
// Run: node test/e2e-remote-client.mjs [main files show link labels down restart two bad local]
//   E2E_TMP            where the run directories go (default: the system's)
//   ELECTRON_OVERRIDE_DIST_PATH  another Electron (electron's own variable)
import { spawn, execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import crypto from 'node:crypto';
import dgram from 'node:dgram';
import fs from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createMockHerdr } from './mock-herdr.mjs';
import { reapOnExit, awaitExit } from './e2e-reap.mjs';

const require = createRequire(import.meta.url);
const electronBin = require('electron');
const { chromium } = require('playwright-core');
const { isFileRel } = require('../src/remote-rules.js');
const here = path.dirname(fileURLToPath(import.meta.url));
const tmpRoot = process.env.E2E_TMP || os.tmpdir();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const waitFor = async (f, ms = 5000, step = 40) => {
  const t = Date.now();
  for (;;) {
    const v = await Promise.resolve().then(f).catch(() => null);
    if (v) return v;
    if (Date.now() - t > ms) return null;
    await sleep(step);
  }
};
let total = 0;
let failed = 0;
const check = (name, ok, detail) => {
  total++;
  if (!ok) failed++;
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail === undefined ? '' : ' — ' + JSON.stringify(detail).slice(0, 260)}`);
};

// ---------- B's web: a site per host, no sockets of its own ----------

const certDir = fs.mkdtempSync(path.join(tmpRoot, 'hc-cert-'));
execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '2', '-subj', '/CN=test', '-keyout', `${certDir}/key.pem`, '-out', `${certDir}/cert.pem`], { stdio: 'ignore' });
const cert = fs.readFileSync(`${certDir}/cert.pem`);
const key = fs.readFileSync(`${certDir}/key.pem`);
const spki = crypto.createHash('sha256').update(new crypto.X509Certificate(cert).publicKey.export({ type: 'spki', format: 'der' })).digest('base64');
const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64');

function site(log, ctx) {
  return (req, res) => {
    const u = new URL(req.url, 'http://x');
    const host = req.headers.host || '';
    log({ t: 'req', host, path: u.pathname, cookie: req.headers.cookie || '', auth: req.headers.authorization || '', proxyAuth: !!req.headers['proxy-authorization'], peer: req.socket.getPeerCertificate ? Object.keys(req.socket.getPeerCertificate() || {}).length : null });
    const send = (b, type = 'text/html', code = 200, h = {}) => {
      res.writeHead(code, { 'content-type': type, 'cache-control': 'no-store', 'access-control-allow-origin': '*', ...h });
      res.end(b);
    };
    const C = ctx.canary;
    switch (u.pathname) {
      case '/img.png':
        return send(PNG, 'image/png');
      case '/to':
        return send('', 'text/plain', Number(u.searchParams.get('code') || 302), { location: u.searchParams.get('u') });
      case '/meta':
        return send(`<title>meta</title><meta http-equiv=refresh content="0;url=${u.searchParams.get('u')}">`);
      case '/js':
        return send(`<title>js</title><script>location.href=${JSON.stringify(u.searchParams.get('u'))}</script>`);
      case '/basic':
        return send('<title>401</title>', 'text/html', 401, { 'www-authenticate': 'Basic realm="origin"' });
      case '/setcookie':
        return send('<title>cookie</title>', 'text/html', 200, { 'set-cookie': `who=${u.searchParams.get('who')}; Max-Age=600` });
      case '/auth':
        return send(`<title>consent</title><script>setTimeout(()=>location.assign('/authredir'),900)</script>`);
      case '/authredir':
        return send('', 'text/plain', 302, { location: 'http://app.test/callback?code=9' });
      case '/callback':
        return send(`<title>cb</title><script>opener&&opener.postMessage({code:new URLSearchParams(location.search).get('code')},location.origin);setTimeout(()=>window.close(),400)</script>`);
      case '/first':
        return send(`<title>first</title><img id=i src="https://cdn.test/img.png"><script>window.ev=[];const a=new WebSocket('wss://ws.test/e');a.onmessage=m=>ev.push('wss '+m.data);a.onclose=e=>ev.push('close '+e.code);const b=new WebSocket('ws://localhost:${C}/e');b.onmessage=m=>ev.push('ws '+m.data);b.onerror=()=>ev.push('wserr')</script>`);
      case '/tall':
        return send(`<title>tall</title><body style="margin:0;background:rgb(10,200,30)"><button id=b style="width:200px;height:50px" onclick="window.n=(window.n||0)+1">b</button><input id=q>`);
      default:
        return send(`<title>app ${host}</title><h1>app</h1><button id=oauth onclick="window.h=window.open('https://idp.test/auth','a','popup,width=400,height=400')">o</button><a id=blank target=_blank href="http://other.test/tall">b</a><script>addEventListener('message',e=>window.got=e.data)</script>`);
    }
  };
}
function wsUp(log) {
  return (req, sock) => {
    log({ t: 'ws', host: req.headers.host });
    const k = crypto.createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    sock.write(`HTTP/1.1 101 X\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${k}\r\n\r\n`);
    sock.write(Buffer.concat([Buffer.from([0x81, 5]), Buffer.from('hello')]));
    sock.on('error', () => {});
  };
}
// The helper's proxy as the contract has it: Basic with the secret, herdr-warm.invalid answered
// by itself, any other .invalid 502.
async function startProxy(log, ctx, secretRef, port = 0) {
  const handler = site(log, ctx);
  const h = http.createServer(handler);
  h.on('upgrade', wsUp(log));
  const s = https.createServer({ key, cert }, handler);
  const mtls = https.createServer({ key, cert, requestCert: true, rejectUnauthorized: false }, handler); // asks for a client certificate
  for (const x of [s, mtls]) {
    x.on('upgrade', wsUp(log));
    x.on('tlsClientError', () => {});
  }
  const socks = new Set();
  const ok = (r) => r.headers['proxy-authorization'] === 'Basic ' + Buffer.from('herdr:' + secretRef.v).toString('base64');
  const p = http.createServer((req, res) => {
    log({ t: 'abs', url: req.url, ok: ok(req) });
    if (!ok(req)) {
      res.writeHead(407, { 'proxy-authenticate': 'Basic realm="herdr"' });
      return res.end('407');
    }
    const u = new URL(req.url);
    delete req.headers['proxy-authorization'];
    if (u.host === 'herdr-warm.invalid') {
      res.writeHead(204, { 'x-herdr-proxy': 'test' });
      return res.end();
    }
    if (u.hostname.endsWith('.invalid') || u.hostname === 'down.test') {
      res.writeHead(502, { 'content-type': 'text/html' });
      return res.end('<title>502</title>');
    }
    req.url = u.pathname + u.search;
    req.headers.host = u.host;
    h.emit('request', req, res);
  });
  p.on('connect', (req, sock, head) => {
    sock.on('error', () => {});
    log({ t: 'connect', url: req.url, ok: ok(req) });
    if (!ok(req)) return sock.end('HTTP/1.1 407 x\r\nProxy-Authenticate: Basic realm="herdr"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n');
    if (/^down\.test/.test(req.url)) return sock.end('HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n');
    sock.write('HTTP/1.1 200 OK\r\n\r\n');
    if (head.length) sock.unshift(head);
    sock.once('data', (b) => {
      sock.pause();
      sock.unshift(b);
      if (b[0] === 0x16) return (/^mtls\.test/.test(req.url) ? mtls : s).emit('connection', sock);
      h.emit('connection', sock);
      process.nextTick(() => sock.resume());
    });
  });
  p.on('connection', (k) => {
    socks.add(k);
    k.on('close', () => socks.delete(k));
    k.on('error', () => {});
  });
  p.on('clientError', (_e, k) => k.destroy());
  await new Promise((r, j) => {
    p.once('error', j);
    p.listen(port, '127.0.0.1', r);
  });
  return {
    port: p.address().port,
    close: () =>
      new Promise((r) => {
        for (const k of socks) k.destroy();
        p.close(r);
      }),
  };
}

// ---------- B's files and the helper in front of them ----------

// link.file, host_state with fresh tokens (§4.2), link_down on what is relayed. Files are a
// map here: nothing of them is on this Mac's disk.
const RELAYED = new Set(['browser.host_state', 'browser.host_sync', 'browser.host_report', 'browser.send_page', 'tab.focus', 'browser.page_message']);
const CHUNK = 256 * 1024;
function farSide(mock) {
  const far = { epoch: 1, down: false, files: new Map(), tokens: new Map(), calls: [], asked: [], slowMs: 0, active: 0, peak: 0 };
  far.mint = () => {
    const a = 'abcdefghijklmnopqrstuvwxyz0123456789';
    const t = 'k' + [...crypto.randomBytes(23)].map((x) => a[x % 36]).join('');
    far.tokens.set(t, far.epoch);
    return t;
  };
  // a shown file's entry as herdr sends it to the link: a new token each time
  const serve = (e) => {
    const m = e.kind === 'html_view' && /^herdr-file:\/\/[^/]+\/(.*)$/.exec(e.url || '');
    if (!m || e.keep) return e;
    const t = far.mint();
    const { file, ...rest } = e;
    return { ...rest, serve: t, url: `herdr-file://${t}/${m[1]}` };
  };
  mock.state.helper = async (method, params) => {
    far.asked.push(method);
    if (RELAYED.has(method) && far.down) return { error: { code: 'link_down', message: 'the link to devhost1 is down' } };
    if (method === 'browser.host_state') {
      const tabs = [...mock.state.tabs.values()].map((t) => ({
        tab_id: t.tab_id,
        workspace_id: t.workspace_id,
        label: t.label,
        ...(t.browser ? { key: t.browser.key, rev: t.browser.rev, active: t.browser.active, entries: t.browser.entries.map(({ target, view_id, ...e }) => serve(e)) } : {}),
      }));
      return { result: { type: 'browser_host_state', state: { focused_tab_id: mock.state.focused, tabs } } };
    }
    if (method !== 'link.file') return null;
    far.calls.push({ ...params, at: Date.now() });
    if (far.down) return { error: { code: 'link_down', message: 'the link to devhost1 is down' } };
    far.active++;
    far.peak = Math.max(far.peak, far.active);
    await sleep(far.slowMs || 1);
    far.active--;
    if (far.tokens.get(params.token) !== far.epoch) return { result: { status: 403, mime: '', size: 0, data: '', eof: false } };
    const f = far.files.get(decodeURIComponent(params.path));
    if (!f) return { result: { status: 404, mime: '', size: 0, data: '', eof: false } };
    const part = f.data.subarray(params.offset, params.offset + CHUNK);
    return { result: { status: 200, mime: f.mime, size: f.data.length, data: part.toString('base64'), eof: params.offset + part.length >= f.data.length } };
  };
  far.put = (rel, mime, data) => far.files.set(rel, { mime, data: Buffer.from(data) });
  far.linkUp = () => {
    far.down = false;
    for (const s of mock.state.subscribers) s.sock.write(JSON.stringify({ event: 'herdr.link_up', data: { type: 'link_up' } }) + '\n');
  };
  return far;
}

// ---------- one companion ----------

const children = new Set();
reapOnExit(() => [...children]);

async function startCanary() {
  let hits = 0;
  const server = net.createServer((s) => {
    hits++;
    s.on('error', () => {});
    s.end('HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 22\r\n\r\n<title>CANARY</title>\n');
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const port = server.address().port;
  const udp = dgram.createSocket('udp4');
  udp.on('message', () => hits++);
  await new Promise((r) => udp.bind(port, '127.0.0.1', r));
  return { port, hits: () => hits, close: () => (server.close(), udp.close()) };
}

async function rig({ remote = true, proxyUp = true, restore = null, label = 'devhost1' } = {}) {
  const dir = fs.mkdtempSync(path.join(tmpRoot, 'hc-'));
  const canary = await startCanary();
  const log = [];
  const ctx = { canary: canary.port };
  const secretRef = { v: 'secret-' + crypto.randomBytes(8).toString('hex') };
  const px = { cur: await startProxy((e) => log.push({ ...e, at: Date.now() }), ctx, secretRef) };
  px.port = px.cur.port;
  if (!proxyUp) await px.cur.close();
  const homeDir = path.join(dir, 'home');
  fs.mkdirSync(homeDir, { mode: 0o700 });
  const writeProxyFile = (port = px.port) => fs.writeFileSync(path.join(homeDir, 'proxy.json'), JSON.stringify({ port, secret: secretRef.v, label }), { mode: 0o600 });
  writeProxyFile();
  const sock = path.join(dir, 's.sock');
  const mock = createMockHerdr({ socketPath: sock, verifyCdp: true, heartbeatMs: 1000, ttlMs: 8000 });
  const far = remote ? farSide(mock) : null;
  if (restore) restore(mock, far);
  await mock.listen();
  const t0 = Date.now();
  const child = spawn(electronBin, [path.join(here, 'e2e-remote-main.cjs'), '--socket', sock, '--profile-dir', path.join(dir, 'profile'), '--headless', ...(remote ? ['--remote', homeDir] : [])], {
    env: { PATH: process.env.PATH, HOME: dir, TMPDIR: dir, HERDR_COMPANION_HEADLESS: '1', HERDR_COMPANION_TEST_HOOKS: '1', HERDR_SOCKET_PATH: '', E2E_SPKI: spki },
    stdio: ['ignore', 'pipe', 'pipe', 'ipc'],
  });
  children.add(child);
  let out = '';
  child.stdout.on('data', (d) => (out += d));
  child.stderr.on('data', (d) => (out += d));
  const exited = new Promise((r) => child.on('exit', (c) => r(c)));
  const pending = new Map();
  let seq = 0;
  child.on('message', (m) => {
    const p = pending.get(m.id);
    if (!p) return;
    pending.delete(m.id);
    if (m.error) p.reject(new Error(m.error));
    else p.resolve(m.value);
  });
  // an op of the main process (e2e-remote-main.cjs)
  const main = (op, ...args) =>
    new Promise((resolve, reject) => {
      const id = ++seq;
      pending.set(id, { resolve, reject });
      child.send({ id, op, args });
    });
  const lease = await waitFor(() => mock.state.lease, 20000);
  const registeredMs = Date.now() - t0;
  if (!lease) console.log('NO LEASE', out.slice(-2000));
  const port = Number(fs.readFileSync(path.join(dir, 'profile', 'DevToolsActivePort'), 'utf8').split('\n')[0]);
  const attach = () => chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true, timeout: 5000 });
  const b = await attach();
  const pages = () => b.contexts().flatMap((c) => c.pages());
  const ctl = () => pages().find((p) => p.url().startsWith('herdr-companion://control/'));
  const call = (op, a) => ctl().evaluate(([o, x]) => globalThis.__herdrCompanion[o](x), [op, a]);
  const pageOf = (target) =>
    waitFor(async () => {
      for (const p of pages()) {
        // a page blocked in its dialog call answers nothing
        const id = await Promise.race([
          (async () => {
            const s = await p.context().newCDPSession(p);
            const { targetInfo } = await s.send('Target.getTargetInfo');
            await s.detach();
            return targetInfo.targetId;
          })(),
          sleep(1000),
        ]);
        if (id === target) return p;
      }
      return null;
    });
  const open = async (url, extra = {}) => {
    const t = Date.now();
    const o = await call('open', { tab_id: 'w1:t1', url, background: false, ...extra });
    const ms = Date.now() - t;
    return { ...o, ms, p: o && o.target_id ? await pageOf(o.target_id) : null };
  };
  const wcOf = async (target) => {
    // the view's webContents id in the main process: by URL, so only for a URL that is unique
    const p = await pageOf(target);
    return (await main('contents')).find((x) => x.url === p.url());
  };
  const errors = () => mock.state.reports.filter((x) => x.error === 'load_failed');
  return {
    dir, mock, far, main, b, pages, call, open, pageOf, wcOf, errors, log, px, secretRef, canary, attach, registeredMs, t0, writeProxyFile, homeDir,
    out: () => out,
    // the floating bar's page, and what the card shows (its text; null while none is up)
    strip: () => pages().find((p) => p.url().startsWith('herdr-companion://ui/strip.html')),
    card: () =>
      waitFor(async () => {
        const p = pages().find((x) => x.url().startsWith('herdr-companion://ui/dialog.html'));
        return p.evaluate(() => (document.getElementById('card').hidden ? null : { origin: document.getElementById('origin').textContent, message: document.getElementById('message').textContent }));
      }, 3000),
    status: () => pages().find((p) => p.url().startsWith('herdr-companion://ui/strip.html')).evaluate(() => (document.getElementById('status').hidden ? null : document.getElementById('status').textContent)),
    views: () => call('views'),
    async proxyDown() {
      await px.cur.close();
    },
    async proxyUp(p = px.port) {
      px.cur = await startProxy((e) => log.push({ ...e, at: Date.now() }), ctx, secretRef, p);
      return px.cur.port;
    },
    async stop(name) {
      await call('quit').catch(() => {});
      await b.close().catch(() => {});
      const code = await awaitExit(child, exited, 20000);
      children.delete(child);
      await px.cur.close().catch(() => {});
      for (const k of mock.state.sockets) k.destroy();
      await mock.close().catch(() => {});
      check(`${name}: quits by itself`, code === 0, code);
      check(`${name}: no window was shown`, !/\[SHOWN\]/.test(out));
      check(`${name}: no uncaught exception`, !/uncaught|UnhandledPromise/i.test(out), out.split('\n').find((l) => /uncaught|Unhandled/i.test(l)));
      check(`${name}: the secret is not in the log`, !out.includes(secretRef.v));
      if (far) check(`${name}: every file asked for is a clean relative path`, far.calls.every((c) => /^[a-z][a-z0-9]{23}$/.test(c.token) && isFileRel(c.path)), far.calls.filter((c) => !isFileRel(c.path)));
      if (far) check(`${name}: no page_message call`, !far.asked.includes('browser.page_message'));
      if (remote) check(`${name}: canary 0`, canary.hits() === 0, canary.hits());
      canary.close();
      fs.rmSync(dir, { recursive: true, force: true });
    },
  };
}
const ws = (p, url = 'wss://ws.test/e') =>
  p.evaluate(
    (u) =>
      new Promise((r) => {
        const w = new WebSocket(u);
        w.onmessage = (m) => r(m.data);
        w.onclose = (e) => r('close ' + e.code);
        setTimeout(() => r('timeout'), 2500);
      }),
    url,
  );
const img = (p) =>
  p.evaluate(
    () =>
      new Promise((r) => {
        const i = new Image();
        i.onload = () => r('load');
        i.onerror = () => r('error');
        i.src = 'https://cdn.test/img.png?' + Math.random();
      }),
  );
const goto = (p, u) => p.goto(u, { timeout: 5000 }).then((x) => x && x.status(), (e) => e.message.split('\n')[0].replace('page.goto: ', ''));
const titled = (p, t, ms = 4000) => waitFor(async () => (await p.title()) === t, ms);
// A never-shown window draws no frame: captures drive them while an input check runs.
const frames = async (pg, f) => {
  let on = true;
  const loop = (async () => {
    while (on) await pg.screenshot({ timeout: 3000 }).catch(() => {});
  })();
  try {
    return await f();
  } finally {
    on = false;
    await loop;
  }
};

// ---------- scenarios ----------

const scenarios = {
  // start, first page, redirects, ws, popup, input, dialogs, permissions, DevTools, certificates
  async main() {
    const r = await rig();
    const { call, open, main, mock, canary } = r;
    const C = canary.port;
    check('registers soon', r.registeredMs < 6000, r.registeredMs);
    const warm = r.log.find((e) => e.t === 'abs' && /herdr-warm/.test(e.url) && e.ok);
    check('the web session was warm before the register', warm && mock.state.registers.length === 1 && warm.at <= mock.state.registers[0].at);
    check('switches', JSON.stringify(await main('switches')) === JSON.stringify({ 'disable-quic': '', 'host-resolver-rules': `MAP herdr-proxy.invalid:${r.px.port} 127.0.0.1:${r.px.port}, MAP * ~NOTFOUND`, 'proxy-server': `http://herdr-proxy.invalid:${r.px.port}`, 'proxy-bypass-list': '<-loopback>' }), await main('switches'));
    for (const s of ['default', 'web', 'ui', 'fresh-partition']) {
      const got = await waitFor(async () => { const x = await main('session', s); return x.proxy !== 'DIRECT' && x; }, 2000);
      check(`session ${s}: proxied, loopback too, file: handled`, got.file && /PROXY herdr-proxy\.invalid/.test(got.proxy) && /PROXY herdr-proxy\.invalid/.test(got.proxyName) && !/DIRECT/.test(got.proxy + got.proxyName), got);
    }

    const f = await open('http://app.test/first');
    check('open answers a target id', f.target_id && f.p, f.target_id);
    check('first page', f.p && (await titled(f.p, 'first')), f.p && f.p.url());
    check('cold cross-origin https image', await waitFor(() => f.p.evaluate(() => document.getElementById('i').naturalWidth === 1)));
    const ev = await waitFor(async () => {
      const e = await f.p.evaluate(() => window.ev);
      return e.length >= 2 && e;
    });
    check('cold wss, and ws to localhost on the canary port (the proxy\'s)', ev && ev.includes('wss hello') && ev.includes('ws hello'), ev);
    const s = await open('https://secure.test/');
    check('https page', s.p && (await titled(s.p, 'app secure.test')));

    // every way to the canary's port ends at the proxy
    const hosts = [`localhost:${C}`, `127.0.0.1:${C}`, `127.1:${C}`, `2130706433:${C}`, `[::1]:${C}`, `0.0.0.0:${C}`, `a.localhost:${C}`, `herdr-proxy.invalid:${C}`];
    for (const h of hosts) {
      const st = await goto(s.p, `http://${h}/`);
      check(`page http://${h}/ is the proxy's`, h.includes('.invalid') ? st === 502 : st === 200 && (await s.p.title()).startsWith('app '), st);
    }
    for (const [name, u] of [['302', `/to?code=302&u=`], ['307', `/to?code=307&u=`], ['meta', `/meta?u=`], ['js', `/js?u=`]]) {
      await goto(s.p, `http://app.test${u}${encodeURIComponent(`http://127.0.0.1:${C}/landed-${name}`)}`);
      check(`${name} redirect to the canary's port ends at the proxy`, await waitFor(async () => s.p.url().endsWith(`/landed-${name}`) && (await s.p.title()).startsWith('app 127.0.0.1')), s.p.url());
    }
    const st2 = await goto(s.p, `https://secure.test/to?u=${encodeURIComponent(`http://localhost:${C}/landed`)}`);
    check('https redirect to http localhost', st2 === 200 && /localhost/.test(s.p.url()), [st2, s.p.url()]);
    await s.p
      .evaluate(
        async (c) => {
          for (const u of [`http://127.0.0.1:${c}/f`, `http://localhost:${c}/f`]) {
            await fetch(u, { mode: 'no-cors' }).catch(() => {});
            navigator.sendBeacon(u, 'x');
            for (const rel of ['preconnect', 'dns-prefetch', 'prefetch', 'preload']) document.head.append(Object.assign(document.createElement('link'), { rel, href: u, as: 'image' }));
            document.body.append(Object.assign(document.createElement('iframe'), { src: u }));
          }
          const pc = new RTCPeerConnection({ iceServers: [{ urls: `stun:127.0.0.1:${c}` }] });
          window.cands = [];
          pc.createDataChannel('x');
          pc.onicecandidate = (e) => e.candidate && window.cands.push(e.candidate.candidate);
          await pc.setLocalDescription(await pc.createOffer());
          await new Promise((r2) => setTimeout(r2, 800));
        },
        C,
      )
      .catch((e) => console.log(e.message.split('\n')[0]));
    check('WebRTC: no candidates', (await s.p.evaluate(() => window.cands.length)) === 0);
    check('a cold session\'s first request never goes direct', !/status 200/.test(await main('cold', 'cold-1', `http://127.0.0.1:${C}/cold`)) && canary.hits() === 0);
    const gated = await main('gate', 'cold-2', `http://127.0.0.1:${C}/gated`);
    check('a second session has its own gate, then loads', gated.open && gated.then === 'status 200', gated);

    // popup, target=_blank
    const a = await open('http://app.test/');
    await titled(a.p, 'app app.test');
    await frames(a.p, () => a.p.locator('#oauth').click({ force: true, timeout: 4000 }).catch((e) => console.log(e.message.split('\n')[0])));
    const pops = await waitFor(async () => {
      const p = await call('popups');
      return p.length && p;
    }, 3000);
    check('an OAuth popup is a hidden child window', pops && pops[0].parent_is_host && !pops[0].visible, pops && pops[0].url);
    const popWc = await waitFor(async () => (await main('contents')).find((x) => x.type === 'window' && /idp\.test|app\.test\/callback/.test(x.url)), 2000, 20);
    check('... without non-proxied UDP, in the web session', popWc && popWc.webrtc === 'disable_non_proxied_udp' && popWc.partition === 'web' && popWc.file, popWc);
    check('... its cross-site redirect posts back to the opener', await waitFor(() => a.p.evaluate(() => window.got && window.got.code === '9')));
    check('... and it closes itself', await waitFor(async () => (await call('popups')).length === 0));
    const before = (await call('views')).length;
    await frames(a.p, () => a.p.locator('#blank').click({ force: true, timeout: 4000 }).catch((e) => console.log(e.message.split('\n')[0])));
    check('target=_blank opens a tab, and it loads', (await waitFor(async () => (await call('views')).length === before + 1)) && (await waitFor(() => r.pages().find((p) => p.url() === 'http://other.test/tall'))));

    // every page and bar: no DevTools, the UDP policy, file: refused
    const all = await main('contents');
    const opened = await main('devtools', all.map((x) => x.id), true);
    check('DevTools opens on no page, bar or card', all.length >= 6 && opened.length === 0, opened);
    check('every WebContents: WebRTC policy, file: handled', all.every((x) => x.webrtc === 'disable_non_proxied_udp' && x.file));
    check('the bars\' pages are the ui session', all.filter((x) => x.url.startsWith('herdr-companion:')).every((x) => x.partition === 'ui'));

    // permissions: checks and requests, nothing asked in the strip
    const perm = await a.p.evaluate(async () => {
      const q = async (name) => (await navigator.permissions.query({ name }).catch((e) => ({ state: e.name }))).state;
      const out = {};
      for (const n of ['geolocation', 'notifications', 'camera', 'microphone', 'clipboard-read', 'clipboard-write']) out[n] = await q(n);
      return out;
    });
    check('permission checks denied (secure-context ones on https)', perm.geolocation === 'denied' && perm.notifications === 'denied', perm);
    await goto(s.p, 'https://secure.test/');
    const sp = await s.p.evaluate(async () => {
      const out = {};
      for (const n of ['geolocation', 'notifications', 'camera', 'microphone', 'clipboard-read', 'clipboard-write']) out[n] = (await navigator.permissions.query({ name: n }).catch((e) => ({ state: e.name }))).state;
      out.media = await navigator.mediaDevices.getUserMedia({ audio: true }).then(() => 'GRANTED', (e) => e.name);
      out.notify = await Notification.requestPermission();
      out.geo = await new Promise((r2) => navigator.geolocation.getCurrentPosition(() => r2('GRANTED'), (e) => r2('code ' + e.code)));
      return out;
    });
    check('https: every check and request denied', ['geolocation', 'notifications', 'camera', 'microphone', 'clipboard-read', 'clipboard-write'].every((n) => sp[n] === 'denied') && sp.media === 'NotAllowedError' && sp.notify === 'denied' && sp.geo === 'code 1', sp);
    await s.p.evaluate(() => {
      const i = document.createElement('iframe');
      i.src = 'herdr-no-such-app:x';
      document.body.append(i);
    });
    await sleep(300);
    const recs = await main('permissions');
    check('every permission request was answered no at once', recs.length > 0 && recs.every((x) => x.verdict === false), recs.map((x) => `${x.permission}:${x.verdict}`));

    // certificates and an origin's own login
    const m = await goto(s.p, 'https://mtls.test/');
    const mreq = r.log.filter((e) => e.t === 'req' && e.host === 'mtls.test');
    check('a server that asks for a client certificate gets none', m === 200 && mreq.length && mreq.every((e) => e.peer === 0), [m, await main('asked')]);
    const ce = await main('cert');
    check('select-client-certificate is answered with none', ce.prevented && ce.answered && ce.answered.length === 0, ce);
    const st401 = await goto(s.p, 'https://secure.test/basic');
    check('an origin\'s login is not answered', st401 === 401 && r.log.every((e) => e.t !== 'req' || !e.auth), st401);
    check('the proxy\'s secret never reaches an origin', r.log.every((e) => e.t !== 'req' || !e.proxyAuth));

    // input, capture, dialogs, history, errors
    const t = await open('http://app.test/tall');
    await titled(t.p, 'tall');
    check('click and type', (await frames(t.p, () => t.p.locator('#b').click({ timeout: 4000 }).then(() => t.p.locator('#q').pressSequentially('abc', { timeout: 3000 })).then(() => 'ok', (e) => e.message.split('\n')[0]))) === 'ok' && (await t.p.evaluate(() => [window.n, document.getElementById('q').value].join())) === '1,abc');
    await call('select', s.view_id);
    await sleep(200);
    const held = await call('paint', { view_id: t.view_id, ms: 4000 });
    const shot = await t.p.screenshot({ timeout: 5000 }).then((x) => x.length, (e) => e.message.split('\n')[0]);
    check('hidden view: paint hold and screenshot', held.held && shot > 1000, [held, shot]);
    await call('paint', { view_id: t.view_id, ms: 0 });
    const pr = a.p.evaluate(() => confirm('sure?'));
    await waitFor(async () => (await call('dialogs')).dialogs.length);
    await call('dialog', { view_id: a.view_id, accept: true });
    check('confirm', (await pr) === true);
    await goto(a.p, 'http://app.test/tall');
    await a.p.goBack({ timeout: 5000 });
    check('goto, back, reload', a.p.url() === 'http://app.test/' && (await a.p.reload({ timeout: 5000 })).status() === 200);
    await open('https://down.test/');
    check('https failure reaches herdr as load_failed', await waitFor(() => r.errors().some((x) => /ERR_TUNNEL_CONNECTION_FAILED/.test(x.text) && /down\.test/.test(x.text))));
    const e2 = await open('http://down.test/');
    check('http 502 is shown as a page', e2.p && (await titled(e2.p, '502')));
    check('only the failing page was reported', r.errors().length === 1, r.errors().map((x) => x.text));
    const un = r.log.filter((e) => e.ok === false).length;
    check('unauthenticated requests: only the warm-up probes\' first ones', un <= 4 && r.log.filter((e) => e.ok === false).every((e) => /herdr-warm/.test(e.url) || /cold/.test(e.url)), [un, r.log.filter((e) => e.ok === false).map((e) => e.url)]);
    await r.stop('main');
  },

  // file: in every session, from every side
  async files() {
    const secretFile = path.join(certDir, 'cert.pem');
    const fileUrl = 'file://' + secretFile;
    const htmlFile = path.join(certDir, 'view.html');
    fs.writeFileSync(htmlFile, '<title>LEAK</title><h1>leak</h1>');
    const entries = [
      { id: 'b1', kind: 'web', url: fileUrl, title: 'restored file', opened_by: 'agent:w1:p1' },
      { id: 'b2', kind: 'html_view', url: 'file://' + htmlFile, file: htmlFile, title: 'restored view', opened_by: 'agent:w1:p1' },
      { id: 'b3', kind: 'html_view', url: 'file://' + htmlFile, file: htmlFile, allow_network: true, title: 'restored net view', opened_by: 'agent:w1:p1' },
      { id: 'b4', kind: 'web', url: 'http://app.test/tall', title: 'restored web', opened_by: 'agent:w1:p1' },
      { id: 'b5', kind: 'html_view', url: 'herdr-file://x/sub/shown.html', title: 'restored shown file', opened_by: 'agent:w1:p1', page_api: true, allow_network: true },
      { id: 'b6', kind: 'html_view', keep: true, serve: 'k'.repeat(24), url: `herdr-file://${'z'.repeat(24)}/sub/shown.html`, title: 'another token', opened_by: 'agent:w1:p1' },
      { id: 'b7', kind: 'web', url: `herdr-file://${'z'.repeat(24)}/sub/shown.html`, title: 'a web page at a file', opened_by: 'agent:w1:p1' },
    ];
    const r = await rig({
      restore: (mock, far) => {
        far.put('sub/shown.html', 'text/html', '<title>SHOWN</title><script>window.api = typeof window.herdr</script>');
        mock.state.tabs.get('w1:t2').browser = { key: 'g-1', rev: 1, active: 'b4', entries };
      },
    });
    const { call, open, main } = r;
    const a = await open('http://app.test/');
    await titled(a.p, 'app app.test');
    check('CDP goto file: in the web session', /ERR_/.test(await goto(a.p, fileUrl)), a.p.url());
    await goto(a.p, 'http://app.test/');
    for (const u of [fileUrl, 'FILE://' + secretFile, 'view-source:' + fileUrl]) {
      await sleep(300); // Electron's own did-finish-load of the goto before would settle this loadURL
      const c = await r.wcOf(a.target_id);
      const got = await main('load', c.id, u);
      const body = await a.p.evaluate(() => document.documentElement.innerText).catch(() => '');
      check(`loadURL ${u.slice(0, 18)}… in the web session`, /ERR_/.test(got[0]) && !/BEGIN CERT/.test(body), got);
      await goto(a.p, 'http://app.test/');
    }
    for (const s of ['default', 'web', 'ui', 'fresh']) check(`fetch file: in session ${s}`, /ERR_|fail/i.test(await main('fetchFile', s, fileUrl)));
    check('net.fetch file: without the bypass', /ERR_|fail/i.test(await main('netDefault', fileUrl)));
    await titled(a.p, 'app app.test');
    await a.p.evaluate(() => {
      window.open('https://idp.test/', 'a', 'popup,width=300,height=300');
    });
    const pop = await waitFor(async () => (await main('contents')).find((x) => x.type === 'window' && /idp\.test/.test(x.url)));
    check('DevTools does not stay open on a popup', pop && (await main('devtools', [pop.id])).length === 0);
    const pg = pop && (await main('load', pop.id, fileUrl));
    check('loadURL file: in a popup', pg && /ERR_/.test(pg[0]), pg);
    const pageTries = await a.p.evaluate(async (u) => {
      const out = [];
      out.push(await fetch(u).then(() => 'LOADED', () => 'no'));
      out.push(await new Promise((x) => Object.assign(document.createElement('img'), { onload: () => x('LOADED'), onerror: () => x('no'), src: u })));
      const w = window.open(u);
      out.push(w ? 'OPENED' : 'no');
      return out;
    }, fileUrl);
    check('a page\'s own file: tries', pageTries.every((x) => x === 'no'), pageTries);
    const before = (await call('views')).length;
    const refused = await call('show', { file: htmlFile, tab_id: 'w1:t1', opened_by: 'agent:w1:p1', focus: false });
    check('show of a local file: refused, no view', refused.error && refused.error.code === 'host_refused' && (await call('views')).length === before, refused);
    const v = await call('showRemote', { tab_id: 'w1:t1', opened_by: 'agent:w1:p1', token: r.far.mint(), rel: 'sub/shown.html', focus: false, title: null });
    const vp = v.target_id && (await r.pageOf(v.target_id));
    check('a shown file of the other machine loads', vp && (await titled(vp, 'SHOWN')), v);
    const vg = await goto(vp, 'file://' + htmlFile);
    check('CDP goto file: in the html session', /ERR_/.test(vg), vg);
    // the agents' restored pages loaded by themselves
    const rest = await waitFor(async () => {
      const x = (await call('views')).filter((y) => y.tab_id === 'w1:t2');
      return x.length === 5 && x.every((y) => y.live && y.target_id) && x;
    }, 6000);
    check('restored: html views without a serve token are dropped, the others are live', rest && rest.map((x) => x.id).join() === 'b1,b4,b5,b6,b7', (await call('views')).map((x) => x.id));
    const urls = Object.fromEntries((rest || []).map((x) => [x.id, x.url]));
    check('... a web page at file: or herdr-file:, and a file of another token, are blank', ['b1', 'b6', 'b7'].every((id) => urls[id] === 'about:blank'), urls);
    const b5 = rest && (await r.pageOf(rest.find((x) => x.id === 'b5').target_id));
    check('... a shown file loads under its serve token', b5 && (await titled(b5, 'SHOWN')) && /^herdr-file:\/\/k[a-z0-9]{23}\/sub\/shown\.html$/.test(b5.url()), b5 && b5.url());
    check('... without page API or network, whatever the entry says', b5 && (await b5.evaluate(() => window.api)) === 'undefined' && (await b5.evaluate(() => fetch('http://app.test/leak').then(() => 'LOADED', () => 'no'))) === 'no' && !r.log.some((e) => e.path === '/leak'));
    check('herdr-file: in the web session: CDP goto and a page\'s own location are refused', /ERR_/.test(await goto(a.p, b5.url())) && (await a.p.evaluate((u) => { location.href = u; return 1; }, b5.url()).catch(() => 1)) && (await sleep(300), (await a.p.title().catch(() => '')) !== 'SHOWN'));
    await goto(a.p, 'http://app.test/');
    let leaks = 0;
    for (const x of rest || []) {
      const p = await r.pageOf(x.target_id);
      const body = p ? await p.evaluate(() => document.title + document.documentElement.innerText).catch(() => '') : '';
      if (/LEAK|leak|BEGIN CERT/.test(body)) leaks++;
    }
    check('restored file: entries show nothing (web, html, html with network)', leaks === 0);
    check('... and the restored web page loads', await waitFor(() => r.pages().find((p) => p.url() === 'http://app.test/tall')));
    const strip = r.pages().find((p) => p.url().startsWith('herdr-companion://ui/strip.html'));
    check('the bar is alive', strip && (await strip.evaluate(() => document.styleSheets.length > 0)));
    await r.stop('files');
  },

  // shown files of the other machine (herdr-file): relative files, no network, one token a page
  async show() {
    const r = await rig();
    const { call, open, main, far, mock } = r;
    const big = crypto.randomBytes(700 * 1024 + 13);
    const put = (rel, mime, text) => far.put(rel, mime, text);
    put('report/in dex.html', 'text/html; charset=utf-8', `<!doctype html><title>report</title><link rel=stylesheet href="css/a.css"><h1 id=h>hi</h1><img id=i src="pix%20%C3%BC.png"><a id=next href="two.html">two</a><a id=web href="http://app.test/tall">web</a><script src="app.js"></script><script type=module src="mod.mjs"></script><iframe id=f src="frame.html"></iframe>`);
    put('report/css/a.css', 'text/css', '@import "b.css"; h1 { color: rgb(1, 2, 3) }');
    put('report/css/b.css', 'text/css', 'h1 { margin: 7px }');
    put('report/app.js', 'text/javascript', 'window.app = 1; window.api = typeof window.herdr; window.rtc = typeof window.RTCPeerConnection');
    put('report/mod.mjs', 'text/javascript', 'import { n } from "./dep.mjs"; window.mod = n; window.dyn = (await import("./dep.mjs")).n');
    put('report/dep.mjs', 'text/javascript', 'export const n = 42');
    put('report/pix ü.png', 'image/png', PNG);
    put('report/data.json', 'application/json', '{"a":1}');
    put('report/big.bin', 'application/octet-stream', big);
    put('report/frame.html', 'text/html', '<title>frame</title><p>framed</p>');
    put('report/two.html', 'text/html', '<title>two</title><a id=back href="in%20dex.html">b</a>');
    put('report/odd.txt', 'text/plain\r\nx: y', 'odd');
    put('report/empty.txt', 'text/plain', '');
    for (let i = 0; i < 40; i++) put(`many/${i}.json`, 'application/json', String(i));
    put('many/index.html', 'text/html', '<title>many</title><script>Promise.all([...Array(40).keys()].map((i) => fetch(i + ".json").then((x) => x.json()))).then((x) => (window.sum = x.reduce((a, b) => a + b)))</script>');
    const other = far.mint();
    const showRemote = (rel, a = {}) => call('showRemote', { tab_id: 'w1:t1', opened_by: 'agent:w1:p1', token: far.mint(), rel, focus: false, title: null, ...a });

    const token = far.mint();
    for (const [name, a] of [
      ['an uppercase token', { token: 'K' + token.slice(1) }], ['a short token', { token: token.slice(1) }], ['no token', { token: null }], ['a digit first', { token: '1'.repeat(24) }],
      ['an absolute path', { rel: '/etc/hosts' }], ['an empty path', { rel: '' }], ['no path', { rel: null }], ['..', { rel: '../x.html' }], ['%2e%2E', { rel: 'a/%2e%2E/x.html' }], ['.', { rel: './x.html' }],
      ['%2f', { rel: 'a%2fx.html' }], ['%5c', { rel: 'a%5Cx.html' }], ['\\', { rel: 'a\\x.html' }], ['%00', { rel: 'x%00.html' }], ['a query', { rel: 'x.html?y' }], ['a fragment', { rel: 'x.html#y' }], ['a long path', { rel: 'x/'.repeat(1100) + 'y' }],
    ]) {
      const o = await showRemote('x.html', { token, ...a });
      check(`showRemote refuses ${name}`, o.error && o.error.code === 'invalid_request', o);
    }
    check('... and makes no view, asks for no file', (await call('views')).length === 0 && far.calls.length === 0);
    const un = await showRemote('report/in%20dex.html', { tab_id: 'w9:t9' });
    check('an unknown herdr tab: tab_unknown', un.error && un.error.code === 'tab_unknown', un);

    // on the marker page at once, however slow the file
    far.slowMs = 2500;
    const t0 = Date.now();
    const v = await showRemote('report/in%20dex.html', { allow_network: true, page_api: true, opener: 'o', file: '/etc/hosts', root: '/' });
    const ms = Date.now() - t0;
    const marked = await waitFor(() => r.pages().find((x) => x.url() === `about:blank#herdr-view=${v.view_id}`), 800);
    check('answers at once with a target, on the marker page', ms < 1500 && v.target_id && marked, [ms, v]);
    const p = v.target_id && (await r.pageOf(v.target_id));
    far.slowMs = 0;
    check('the page loads', p && (await titled(p, 'report')), p && p.url());
    await waitFor(() => p.evaluate(() => window.dyn && document.getElementById('i').naturalWidth));
    const got = await p.evaluate(async () => {
      const h = getComputedStyle(document.getElementById('h'));
      const bytes = new Uint8Array(await (await fetch('big.bin')).arrayBuffer());
      const hash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))].map((x) => x.toString(16).padStart(2, '0')).join('');
      localStorage.setItem('a', '1');
      const odd = await fetch('odd.txt');
      return { color: h.color, margin: h.marginTop, img: document.getElementById('i').naturalWidth, app: window.app, mod: window.mod, dyn: window.dyn, json: await (await fetch('data.json')).json(), size: bytes.length, hash, frame: document.getElementById('f').contentDocument.title, ls: localStorage.getItem('a'), odd: odd.headers.get('content-type'), oddx: odd.headers.get('x'), empty: await (await fetch('empty.txt')).text(), secure: isSecureContext, head: await fetch('big.bin', { method: 'HEAD' }).then((x) => x.headers.get('content-length')), post: await fetch('data.json', { method: 'POST', body: 'x' }).then((x) => x.status) };
    });
    check('css, @import, script, modules, image, iframe, fetch, storage', got.color === 'rgb(1, 2, 3)' && got.margin === '7px' && got.img === 1 && got.app === 1 && got.mod === 42 && got.dyn === 42 && got.json.a === 1 && got.frame === 'frame' && got.ls === '1' && got.secure, got);
    check('a file of 700 KiB comes whole, in pulls of 256 KiB', got.size === big.length && got.hash === crypto.createHash('sha256').update(big).digest('hex') && far.calls.filter((c) => c.path === 'report/big.bin').map((c) => c.offset).slice(0, 3).join() === '0,262144,524288', got.size);
    check('HEAD, POST, an empty file, a mime with a line break', got.head === String(big.length) && got.post === 405 && got.empty === '' && got.odd === 'application/octet-stream' && got.oddx === null, got);
    check('paths go out percent-encoded, without the leading slash', far.calls.some((c) => c.path === 'report/in%20dex.html') && far.calls.some((c) => c.path === 'report/pix%20%C3%BC.png'));
    check('no page API, no WebRTC', (await p.evaluate(() => [window.api, window.rtc].join())) === 'undefined,undefined');

    const net1 = await p.evaluate(async (c) => {
      const out = {};
      for (const u of ['http://app.test/leak', 'https://app.test/leak', `http://127.0.0.1:${c}/leak`]) out[u] = await fetch(u, { mode: 'no-cors' }).then(() => 'LOADED', () => 'no');
      out.img = await new Promise((x) => Object.assign(new Image(), { onload: () => x('LOADED'), onerror: () => x('no'), src: 'https://cdn.test/leak.png' }));
      out.ws = await new Promise((x) => { const w = new WebSocket('wss://ws.test/leak'); w.onopen = () => x('LOADED'); w.onerror = () => x('no'); });
      const f = document.createElement('iframe');
      f.src = 'http://app.test/leak';
      document.body.append(f);
      await new Promise((x) => setTimeout(x, 300));
      out.beacon = navigator.sendBeacon('http://app.test/leak', 'x');
      out.file = await fetch('file:///etc/hosts').then(() => 'LOADED', () => 'no');
      out.open = window.open('http://app.test/leak') ? 'OPENED' : 'no';
      return out;
    }, r.canary.port);
    await sleep(300);
    check('no network, no file, no window', Object.entries(net1).every(([k, x]) => k === 'beacon' || x === 'no'), net1);
    check('... and the proxy saw none of it', !r.log.some((e) => /leak/.test(e.path || '') || /leak|ws\.test|cdn\.test/.test(e.url || '')), r.log.slice(-3));

    const asked = far.calls.length;
    const cross = await p.evaluate(async (o) => {
      const f = document.createElement('iframe');
      f.src = `herdr-file://${o}/report/two.html`;
      document.body.append(f);
      await new Promise((x) => setTimeout(x, 400));
      let title = 'opaque';
      try { title = f.contentDocument.title; } catch {}
      return { fetch: await fetch(`herdr-file://${o}/report/data.json`).then((x) => 'status ' + x.status, () => 'no'), title };
    }, other);
    check('another token\'s files: refused here, never asked for', cross.fetch !== 'status 200' && cross.title !== 'two' && far.calls.length === asked, cross);
    const trav = await p.evaluate(async () => {
      const out = {};
      for (const u of ['../../../etc/hosts', '%2e%2e/%2e%2e/%2e%2e/etc/hosts', 'a%2f..%2fb', 'a%5c..%5cb', 'x%00', '..%2f..%2fetc', '/', 'gone.txt']) out[u] = await fetch(u).then((x) => x.status, () => 'no');
      return out;
    });
    check('above the folder, encoded separators, NUL: never asked for; a missing file 404', trav['gone.txt'] === 404 && Object.values(trav).every((x) => x !== 200) && far.calls.every((c) => isFileRel(c.path)), trav);

    // links: a file of the same folder here, the web in a tab of its own, file: nowhere
    const n0 = (await call('views')).length;
    await p.evaluate(() => document.getElementById('web').click());
    check('a link to the web opens a tab, the view stays', (await waitFor(async () => (await call('views')).length === n0 + 1)) && /in%20dex\.html$/.test(p.url()), p.url());
    await p.evaluate(() => { location.href = 'file:///etc/hosts'; });
    await sleep(300);
    check('a file: navigation goes nowhere', /in%20dex\.html$/.test(p.url()), p.url());
    await p.evaluate(() => document.getElementById('next').click());
    check('a link to a file beside it works', await titled(p, 'two'), p.url());
    await p.goBack({ timeout: 4000 });
    check('back', await titled(p, 'report'));
    put('report/two.html', 'text/html', '<title>two again</title>');
    check('CDP goto inside its folder, changed since', (await goto(p, `herdr-file://${far.calls[0].token}/report/two.html`)) === 200 && (await titled(p, 'two again')), p.url());
    check('CDP goto of another token\'s file', (await goto(p, `herdr-file://${other}/report/two.html`)) === 403);

    const m = await showRemote('many/index.html', { title: 'A \u202etitle\u0007\n2', focus: true });
    const mp = await r.pageOf(m.target_id);
    await waitFor(() => mp.evaluate(() => window.sum), 6000);
    check('40 files at once: at most 16 pulls on the way', (await mp.evaluate(() => window.sum)) === 780 && far.peak <= 16 && far.peak > 4, far.peak);
    check('a title is cleaned and stays; opened in its tab', (await call('views')).find((x) => x.view_id === m.view_id).title === 'A title 2', (await call('views')).find((x) => x.view_id === m.view_id).title);
    const wcs = await main('contents');
    check('html views: not the web session, file: handled', wcs.filter((x) => x.url.startsWith('herdr-file:')).every((x) => x.partition === 'other' && x.file), wcs.map((x) => [x.url.slice(0, 30), x.partition]));
    const e = await waitFor(() => { const t = mock.state.tabs.get('w1:t1').browser; return t && t.entries.find((x) => x.view_id === m.view_id && x.id); });
    check('host_sync: the token, no path, no flags', e && e.serve && e.url === `herdr-file://${e.serve}/many/index.html` && !('file' in e) && !e.allow_network && !e.page_api && e.opened_by === 'agent:w1:p1', e);
    const reg = await waitFor(() => r.errors().length === 0);
    check('nothing was reported as failed', reg, r.errors());
    await r.stop('show');
  },

  // the link drops and comes back, another Mac has it
  async link() {
    const r = await rig();
    const { call, open, far, mock } = r;
    far.put('a/page.html', 'text/html', '<title>page</title><p>one</p>');
    far.put('a/x.json', 'application/json', '{"x":1}');
    const s = await call('showRemote', { tab_id: 'w1:t1', opened_by: 'agent:w1:p1', token: far.mint(), rel: 'a/page.html', focus: true, title: null });
    const p = await r.pageOf(s.target_id);
    await titled(p, 'page');
    const t0 = far.calls[0].token;
    const s2 = await call('showRemote', { tab_id: 'w1:t1', opened_by: 'agent:w1:p1', token: far.mint(), rel: 'a/page.html', focus: false, title: null });
    const p2 = await r.pageOf(s2.target_id);
    await titled(p2, 'page');
    const w = await open('http://app.test/', { background: true });
    await titled(w.p, 'app app.test');
    check('linked: no status line', (await waitFor(async () => (await r.status()) === null, 8000)));
    await waitFor(() => mock.state.tabs.get('w1:t1').browser && mock.state.tabs.get('w1:t1').browser.entries.every((x) => x.id));

    // down: a dialog asked before it survives
    const said = p.evaluate(() => confirm('keep me?')).catch((e2) => e2.message);
    check('the card is up', (await r.card()).message === 'keep me?');
    far.down = true;
    mock.state.leaseLink = { state: 'reconnecting' };
    check('the bar says reconnecting', await waitFor(async () => (await r.status()) === 'devhost1: reconnecting', 4000), await r.status());
    check('... sending the page is off', await r.strip().evaluate(() => document.getElementById('send').disabled));
    const n = far.asked.filter((x) => RELAYED.has(x)).length;
    const o = await open('http://app.test/tall', { background: true }); // dirties the group
    check('down: pages still open and load (the proxy is another matter)', o.target_id && (await titled(o.p, 'tall')));
    await open('https://down.test/', { background: true }); // a report
    await sleep(1500);
    check('down: herdr is not asked again and again', far.asked.filter((x) => RELAYED.has(x)).length - n <= 1, far.asked.filter((x) => RELAYED.has(x)).length - n);
    check('down: nothing is reported meanwhile', r.errors().length === 0);
    check('down: a shown file that is loaded again says so', (await p2.reload({ timeout: 4000 })).status() === 502 && await waitFor(async () => /devhost1: no connection/.test(await p2.evaluate(() => document.body.innerText))), p2.url());
    check('down: the card is still up, one registration', (await r.card()).message === 'keep me?' && mock.state.registers.length === 1);

    // back, another epoch: the old tokens are refused
    far.epoch = 2;
    mock.state.leaseLink = { state: 'linked' };
    const seq = mock.state.lease.seq;
    const syncs = mock.state.syncs.length;
    far.linkUp();
    const full = await waitFor(() => mock.state.syncs.slice(syncs).find((x) => x.groups.some((g) => g.entries.length >= 5)));
    check('back: everything is synced again, with a rising seq', full && full.seq > seq, full && [seq, full.seq]);
    check('back: no new registration, the dialog is still up', mock.state.registers.length === 1 && (await r.card()).message === 'keep me?');
    await call('dialog', { view_id: s.view_id, accept: true });
    check('... and answered', (await said) === true, await said);
    const e = full && full.groups.flatMap((g) => g.entries).find((x) => x.view_id === s.view_id);
    check('back: the shown file is synced under its new token', e && e.serve !== t0 && far.tokens.get(e.serve) === 2 && e.url === `herdr-file://${e.serve}/a/page.html`, e);
    check('back: its page keeps its URL, and its files come under the new token', p.url() === `herdr-file://${t0}/a/page.html` && (await p.evaluate(() => fetch('x.json').then((x) => x.status))) === 200 && far.calls.at(-1).token === e.serve, [p.url(), far.calls.at(-1)]);
    check('... a reload too', (await p.reload({ timeout: 4000 })).status() === 200 && (await titled(p, 'page')));
    check('back: the file that had no answer loads by itself', await titled(p2, 'page'), p2.url());
    check('back: what was to report arrives', await waitFor(() => r.errors().some((x) => /down\.test/.test(x.text))), r.errors().map((x) => x.text));
    check('back: the status line is gone', await waitFor(async () => (await r.status()) === null, 8000), await r.status());

    // tokens of another epoch without the event yet: one refused answer brings the new ones
    await sleep(2200); // at most one read in 2 s for refused files
    far.epoch = 3;
    check('a refused token: herdr\'s state is read, the file comes', (await p.evaluate(() => fetch('x.json').then((x) => x.status))) === 200 && far.tokens.get(far.calls.at(-1).token) === 3);
    const states = far.asked.filter((x) => x === 'browser.host_state').length;
    far.put('a/closed.json', 'application/json', '1');
    await p.evaluate(() => Promise.all([...Array(20).keys()].map(() => fetch('gone.json'))));
    check('missing files do not read herdr\'s state', far.asked.filter((x) => x === 'browser.host_state').length === states);

    // another Mac, and back without the event
    far.down = true;
    mock.state.leaseLink = { state: 'in_use', text: 'the browser of devhost1 is on laptop1 (since 14:02); quit herdr+ there' };
    check('another Mac has it: the bar says which', await waitFor(async () => (await r.status()) === 'the browser of devhost1 is on laptop1 (since 14:02); quit herdr+ there', 4000), await r.status());
    await p.evaluate(() => fetch('x.json').then((x) => x.status)).then((st) => check('... a file of it: 502', st === 502, st));
    check('... the line stays', (await r.status()) === 'the browser of devhost1 is on laptop1 (since 14:02); quit herdr+ there');
    const syncs2 = mock.state.syncs.length;
    far.down = false;
    mock.state.leaseLink = { state: 'linked' };
    check('back by a heartbeat alone: synced again', await waitFor(() => mock.state.syncs.length > syncs2 && mock.state.registers.length === 1, 4000));
    check('... the line is gone', await waitFor(async () => (await r.status()) === null, 4000));

    // the helper itself goes and comes back: registers again, as after a herdr restart
    await mock.restart();
    check('the helper restarts: the bar names the machine, then it registers again', await waitFor(() => mock.state.registers.length === 2 && mock.state.lease, 8000), await r.status());
    await r.stop('link');
  },

  // the machine's name on every surface, and the other machine's text on them
  async labels() {
    const r = await rig({
      label: 'leap\u202emini\u0007',
      restore: (mock) => {
        mock.state.tabs.get('w1:t1').label = 'pla\u202enner\n\u001b[1m';
      },
    });
    const { call, open, main, mock } = r;
    const titles = async (re) => waitFor(async () => (await main('titles')).some((x) => re.test(x)) && main('titles'), 3000);
    check('the window names the machine and the tab', (await titles(/^herdr\+ companion — devhost1 — planner \[1m$/)), await main('titles'));
    const a = await open('http://app.test/');
    await titled(a.p, 'app app.test');
    check('... and its pages', await titles(/^herdr\+ companion — devhost1 — planner \[1m \(1\)$/), await main('titles'));
    check('the bar names the machine first', (await r.strip().evaluate(() => { const m = document.getElementById('machine'); return !m.hidden && document.getElementById('bar').firstElementChild === m && m.getBoundingClientRect().width > 20 && m.textContent; })) === 'devhost1');
    for (const [name, prev] of [['docked', { dock: true, kind: 'side' }], ['full', { dock: true, kind: 'full' }]]) {
      await call('strip_preview', prev);
      await sleep(150);
      check(`... ${name} too`, (await r.strip().evaluate(() => !document.getElementById('machine').hidden && document.getElementById('machine').textContent)) === 'devhost1');
    }
    await call('strip_preview', null);
    mock.emit('tab.renamed', { tab_id: 'w1:t1', label: 'new\u202e name\u0000' });
    await r.call('select', a.view_id);
    check('a renamed tab: cleaned', await titles(/^herdr\+ companion — devhost1 — new name \(1\)$/), await main('titles'));

    await a.p.evaluate(() => {
      const w = window.open('https://idp.test/', 'a', 'popup,width=300,height=300');
      setTimeout(() => (w.name = 'x'), 10);
    });
    const pop = await waitFor(async () => { const x = (await call('popups'))[0]; return x && /^devhost1 — idp\.test — /.test(x.title) && x; });
    check('a popup\'s title: the machine, its host, its title', pop, pop || (await call('popups')));
    const pp = await waitFor(() => r.pages().find((x) => x.url() === 'https://idp.test/'));
    await pp.evaluate(() => (document.title = '\u202eherdr+ companion — mbp'));
    check('... whatever the page calls itself', await waitFor(async () => (await call('popups'))[0].title === 'devhost1 — idp.test — herdr+ companion — mbp'), (await call('popups'))[0].title);
    await pp.evaluate(() => window.close()).catch(() => {});

    for (const [type, js] of [['alert', () => alert('hello')], ['confirm', () => confirm('hello')], ['prompt', () => prompt('hello', 'd')]]) {
      const said = a.p.evaluate(js).catch(() => {});
      const card = await r.card();
      check(`the ${type} card names the machine and the site`, card && card.origin === 'devhost1 · http://app.test says' && card.message === 'hello', card);
      await call('dialog', { view_id: a.view_id, accept: false });
      await said;
      await sleep(150);
    }
    await a.p.evaluate(() => document.body.append(Object.assign(document.createElement('iframe'), { srcdoc: '<script>setTimeout(() => alert("in"), 50)</script>' })));
    const emb = await waitFor(async () => { const c = await r.card(); return c.message === 'in' && c; });
    check('an embedded page\'s card too', emb && /^devhost1 · An embedded page/.test(emb.origin), emb);
    await call('dialog', { view_id: a.view_id, accept: true });
    r.far.put('d/i.html', 'text/html', '<title>i</title>');
    const v = await call('showRemote', { tab_id: 'w1:t1', opened_by: 'agent:w1:p1', token: r.far.mint(), rel: 'd/i.html', focus: true, title: null });
    const vp = await r.pageOf(v.target_id);
    await titled(vp, 'i');
    check('a shown file\'s address in the bar: its folder and name, no token', await waitFor(async () => (await r.strip().evaluate(() => document.getElementById('url').value)) === 'd › i.html'), await r.strip().evaluate(() => document.getElementById('url').value));
    const said = vp.evaluate(() => alert('file')).catch(() => {});
    const fc = await waitFor(async () => { const c = await r.card(); return c.message === 'file' && c; });
    check('a shown file\'s card: the machine, no token', fc && fc.origin === 'devhost1 · This page says', fc);
    await call('dialog', { view_id: v.view_id, accept: true });
    await said;

    // a page that is down
    await r.proxyDown();
    await call('select', a.view_id);
    await goto(a.p, 'http://app.test/tall').catch(() => {});
    check('a page that did not load: the bar says so', await waitFor(async () => (await r.status()) === 'devhost1: this page did not load; reload it'), await r.status());
    const b = await open('http://app.test/first');
    check('a page that waits: the bar says so', await waitFor(async () => /^devhost1: no connection for this page/.test(await r.status())), await r.status());
    await r.proxyUp();
    check('... and stops when it loads', (await titled(b.p, 'first')) && (await waitFor(async () => (await r.status()) === null)), await r.status());
    await call('select', a.view_id);
    await goto(a.p, 'http://app.test/tall');
    check('... and after the reload', await waitFor(async () => (await r.status()) === null), await r.status());
    await r.stop('labels');
  },

  // started with the proxy down
  async down() {
    const r = await rig({ proxyUp: false });
    check('registers without the proxy, after its 8 s wait', r.registeredMs > 7000 && r.registeredMs < 14000, r.registeredMs);
    const o = await r.open('http://app.test/first');
    check('open answers at once with a target', o.ms < 2000 && o.target_id, o.ms);
    const o2 = await r.open('http://app.test/tall', { background: true });
    const o3 = await r.open('http://app.test/never', { background: true });
    check('attach works with the gate shut', await r.attach().then((b) => b.close().then(() => true), (e) => e.message));
    check('load_failed reaches herdr, once per page', await waitFor(() => r.errors().length === 3, 4000), r.errors().map((x) => x.text));
    check('... and says why', r.errors().every((x) => /proxy does not answer \(net::ERR_PROXY_CONNECTION_FAILED\)/.test(x.text)));
    const vs = await r.views();
    check('the page stays blank with its URL set', o.p.url().startsWith('about:blank#herdr-view=') && vs.find((x) => x.view_id === o.view_id).url === 'http://app.test/first', o.p.url());
    check('a navigation while down fails closed', /ERR_PROXY_CONNECTION_FAILED/.test(await goto(o3.p, `http://127.0.0.1:${r.canary.port}/`)));
    await r.call('close', o2.view_id);
    const t = Date.now();
    await r.proxyUp();
    check('the proxy is back: the page loads by itself', await waitFor(async () => (await o.p.title()) === 'first', 4000), Date.now() - t);
    check('... with its cold image and ws', await waitFor(async () => { const e = await o.p.evaluate(() => [document.getElementById('i').naturalWidth, window.ev]); return e[0] === 1 && e[1].includes('wss hello') && e[1].includes('ws hello'); }));
    check('the page navigated meanwhile loads that URL, not its first one', await waitFor(() => o3.p.url() === `http://127.0.0.1:${r.canary.port}/`), o3.p.url());
    await sleep(500);
    check('the closed page is not loaded', !r.log.some((e) => e.path === '/tall') && !r.log.some((e) => e.path === '/never'));
    const n = await r.open('https://secure.test/');
    check('a new page loads', n.p && (await titled(n.p, 'app secure.test')));
    await r.stop('down');
  },

  // the proxy goes and comes back; a new secret; another port
  async restart() {
    const r = await rig();
    const a = await r.open('http://app.test/tall');
    await titled(a.p, 'tall');
    check('before: ws and image', (await ws(a.p)) === 'hello' && (await img(a.p)) === 'load');
    await a.p.evaluate(() => {
      window.st = [];
      const w = new WebSocket('wss://ws.test/long');
      w.onopen = () => window.st.push('open');
      w.onclose = (e) => window.st.push('close ' + e.code);
    });
    await waitFor(() => a.p.evaluate(() => window.st.includes('open')));
    await r.proxyDown();
    check('down: an open ws closes', await waitFor(() => a.p.evaluate(() => window.st.some((x) => /close/.test(x)))));
    const b = await r.open('http://app.test/first');
    check('down: open answers at once', b.ms < 2000 && b.target_id, b.ms);
    check('down: load_failed reaches herdr', await waitFor(() => r.errors().some((x) => /ERR_PROXY_CONNECTION_FAILED/.test(x.text) && /\/first/.test(x.text))), r.errors().map((x) => x.text));
    const c = await r.open('http://app.test/later', { background: true });
    check('down: a page opened later waits blank', c.ms < 2000 && (await waitFor(() => c.p.url().startsWith('about:blank#herdr-view='))), c.p.url());
    check('down: loopback fails closed', /ERR_PROXY_CONNECTION_FAILED/.test(await goto(a.p, `http://127.0.0.1:${r.canary.port}/`)));
    check('down: attach works', await r.attach().then((x) => x.close().then(() => true), (e) => e.message));
    const strip0 = r.log.length;
    await r.proxyUp();
    check('back, same secret: both pages load by themselves', (await titled(b.p, 'first')) && (await titled(c.p, 'app app.test')), [b.p.url(), c.p.url()]);
    check('... with the cold image and ws', await waitFor(async () => { const e = await b.p.evaluate(() => [document.getElementById('i').naturalWidth, window.ev]); return e[0] === 1 && e[1].includes('wss hello'); }));
    await sleep(300);
    check('... the page that had shown one is left alone', !r.log.slice(strip0).some((e) => e.path === '/tall' || e.path === '/'), r.log.slice(strip0).filter((e) => e.path).map((e) => e.path));
    check('... and works again', (await goto(a.p, 'http://app.test/tall')) === 200 && (await ws(a.p)) === 'hello' && (await img(a.p)) === 'load');

    // same port, another secret, in the file
    await r.proxyDown();
    r.secretRef.v = 'rotated-' + crypto.randomBytes(6).toString('hex');
    r.writeProxyFile();
    await r.proxyUp();
    const n = await r.open('http://app.test/first?2');
    check('new secret: a new page loads with its cold image and ws', await waitFor(async () => { const e = await n.p.evaluate(() => [document.getElementById('i').naturalWidth, window.ev]); return e[0] === 1 && e[1].includes('wss hello') && e[1].includes('ws hello'); }, 5000));
    check('... an older page\'s ws and image too', (await ws(a.p)) === 'hello' && (await img(a.p)) === 'load');
    await r.proxyDown();
    r.secretRef.v = 'again-' + crypto.randomBytes(6).toString('hex');
    r.writeProxyFile();
    await r.proxyUp();
    check('new secret: an open page\'s navigation is answered, and probes again', (await goto(a.p, 'http://app.test/tall')) === 200 && (await waitFor(async () => (await ws(a.p)) === 'hello')) && (await img(a.p)) === 'load');

    await r.proxyDown();
    const np = await r.proxyUp(0);
    check('another port: fails closed', np !== r.px.port && /ERR_PROXY_CONNECTION_FAILED/.test(await goto(a.p, `http://127.0.0.1:${r.canary.port}/`)));
    await r.stop('restart');
  },

  // two machines' companions at once: each its own proxy, profile and cookies
  async two() {
    const [x, y] = [await rig(), await rig()];
    for (const [r, who] of [[x, 'x'], [y, 'y']]) {
      const p = await r.open(`http://login.test/setcookie?who=${who}`);
      await titled(p.p, 'cookie');
      r.page = p.p;
    }
    for (const [r, who] of [[x, 'x'], [y, 'y']]) {
      await goto(r.page, 'http://login.test/read');
      check(`companion ${who} has its own cookie only`, r.log.filter((e) => e.path === '/read').every((e) => e.cookie === `who=${who}`), r.log.filter((e) => e.path === '/read').map((e) => e.cookie));
      check(`companion ${who} used its own proxy only`, r.log.some((e) => e.path === '/setcookie') && r.log.filter((e) => e.path === '/setcookie').length === 1);
    }
    // the other companion's proxy, without its secret
    const cross = await goto(x.page, `http://127.0.0.1:${y.px.port}/`);
    check('the other proxy\'s port is only a name at x\'s own proxy', cross === 200 && y.log.every((e) => !/^http:\/\/127\.0\.0\.1/.test(e.url || '')), cross);
    const raw = await new Promise((res) => http.get({ host: '127.0.0.1', port: y.px.port, path: 'http://app.test/', headers: { host: 'app.test' } }, (q) => res(q.statusCode)));
    check('a proxy without the secret answers 407 (the rig\'s own)', raw === 407);
    await x.stop('two/x');
    check('y goes on after x quit', (await goto(y.page, 'http://login.test/read')) === 200);
    await y.stop('two/y');
  },

  // --remote that cannot work: never unproxied, never on the user's own profile
  async bad() {
    const dir = fs.mkdtempSync(path.join(tmpRoot, 'hc-'));
    const run = async (args) => {
      const child = spawn(electronBin, [path.join(here, 'e2e-remote-main.cjs'), '--headless', ...args], { env: { PATH: process.env.PATH, HOME: dir, TMPDIR: dir, HERDR_COMPANION_HEADLESS: '1', HERDR_SOCKET_PATH: path.join(dir, 'other.sock') }, stdio: ['ignore', 'pipe', 'pipe'] });
      children.add(child);
      let out = '';
      child.stdout.on('data', (d) => (out += d));
      child.stderr.on('data', (d) => (out += d));
      const code = await Promise.race([new Promise((r) => child.on('exit', r)), sleep(15000).then(() => 'timeout')]);
      if (code !== 'timeout') children.delete(child);
      return { code, out };
    };
    const home = path.join(dir, 'home');
    fs.mkdirSync(home);
    const prof = path.join(dir, 'profile');
    const full = ['--socket', path.join(dir, 's.sock'), '--profile-dir', prof, '--remote', home];
    const a = await run(full);
    check('no proxy.json: exit 2 with the reason', a.code === 2 && /proxy\.json/.test(a.out), a.code);
    fs.writeFileSync(path.join(home, 'proxy.json'), JSON.stringify({ port: 1 }));
    check('a proxy.json without a secret: exit 2', (await run(full)).code === 2);
    fs.writeFileSync(path.join(home, 'proxy.json'), JSON.stringify({ port: 1, secret: 'x' }));
    const b = await run(['--socket', path.join(dir, 's.sock'), '--remote', home]);
    check('no --profile-dir: exit 2', b.code === 2 && /--profile-dir/.test(b.out), b.code);
    const c = await run(['--profile-dir', prof, '--remote', home]);
    check('no --socket: exit 2 (the environment\'s is not taken)', c.code === 2 && /--socket/.test(c.out), c.code);
    check('no profile was made', !fs.existsSync(prof) && !fs.existsSync(path.join(dir, 'Library', 'Application Support', 'herdr-companion')));
    fs.rmSync(dir, { recursive: true, force: true });
  },

  // without --remote: none of it
  async local() {
    const r = await rig({ remote: false });
    const { open, main, call } = r;
    check('registers at once', r.registeredMs < 6000, r.registeredMs);
    check('no switch', Object.values(await main('switches')).every((v) => v === null));
    for (const s of ['default', 'web', 'ui', 'fresh']) check(`session ${s}: file: not handled, loopback direct`, await main('session', s).then((x) => !x.file && x.proxy === 'DIRECT'));
    const a = await open(`http://127.0.0.1:${r.canary.port}/`);
    check('control: the canary is reached without --remote', a.p && (await titled(a.p, 'CANARY')) && r.canary.hits() > 0, r.canary.hits());
    const all = await main('contents');
    const devtools = await main('devtools', all.map((x) => x.id), true);
    check('control: DevTools opens without --remote', devtools.length === all.length, all.filter((x) => !devtools.includes(x.id)).map((x) => x.url));
    check('WebRTC policy as before', all.every((x) => x.webrtc === 'default'), all.map((x) => x.webrtc));
    const g = a.p.evaluate(() => new Promise((res) => navigator.geolocation.getCurrentPosition(() => res('GRANTED'), (e) => res('code ' + e.code), { timeout: 3000 }))).catch(() => {});
    const asked = await waitFor(async () => (await main('permissions')).find((x) => x.permission === 'geolocation'));
    check('a permission is asked, not denied at once', asked && asked.verdict === 'pending', asked);
    await goto(a.p, `http://127.0.0.1:${r.canary.port}/2`);
    await g;
    const htmlFile = path.join(certDir, 'local.html');
    fs.writeFileSync(htmlFile, '<title>LOCAL</title>');
    const v = await call('show', { file: htmlFile, tab_id: 'w1:t1', opened_by: 'agent:w1:p1', focus: false });
    const vp = v.target_id && (await r.pageOf(v.target_id));
    check('show of a local file works', vp && (await titled(vp, 'LOCAL')));
    check('showRemote is unknown, herdr-file: is no scheme', (await call('showRemote', { token: 'k'.repeat(24), rel: 'x.html' })).error.code === 'unknown_op' && /ERR_UNKNOWN_URL_SCHEME|ERR_ABORTED|ERR_FAILED/.test(await goto(a.p, `herdr-file://${'k'.repeat(24)}/x.html`)));
    check('no machine in the bar or the titles', (await r.strip().evaluate(() => document.getElementById('machine').hidden)) && (await main('titles')).every((x) => /^herdr\+ companion( — planner \(\d\))?$/.test(x)), await main('titles'));
    const said = a.p.evaluate(() => alert('hi')).catch(() => {});
    const card = await call('select', a.view_id).then(() => r.card());
    check('... nor on the card', card && card.origin === `http://127.0.0.1:${r.canary.port} says`, card);
    await call('dialog', { view_id: a.view_id, accept: true });
    await said;
    check('the proxy saw nothing', r.log.length === 0, r.log.length);
    await r.stop('local');
  },
};

const wanted = process.argv.slice(2);
for (const name of wanted.length ? wanted : Object.keys(scenarios)) {
  console.log(`--- ${name}`);
  try {
    await scenarios[name]();
  } catch (e) {
    check(`${name}: no exception`, false, e.stack);
  }
}
fs.rmSync(certDir, { recursive: true, force: true });
console.log(`\n${total - failed}/${total} passed`);
process.exit(failed ? 1 : 0);
