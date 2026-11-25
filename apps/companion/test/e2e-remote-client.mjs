// The companion's `--remote` mode (src/remote.js) against the mock herdr, an authenticating
// proxy that answers every page itself (it dials nothing) and a canary on this Mac's loopback
// that must never be reached. Hidden: one Electron per scenario, each with a hard timer
// (e2e-remote-main.cjs), a fresh profile, no clipboard, no download.
//
// Run: node test/e2e-remote-client.mjs [main files down restart two bad local]
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

async function rig({ remote = true, proxyUp = true, restore = null } = {}) {
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
  const writeProxyFile = (port = px.port) => fs.writeFileSync(path.join(homeDir, 'proxy.json'), JSON.stringify({ port, secret: secretRef.v }), { mode: 0o600 });
  writeProxyFile();
  const sock = path.join(dir, 's.sock');
  const mock = createMockHerdr({ socketPath: sock, verifyCdp: true, heartbeatMs: 1000, ttlMs: 8000 });
  if (restore) restore(mock);
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
        const s = await p.context().newCDPSession(p);
        const { targetInfo } = await s.send('Target.getTargetInfo');
        await s.detach();
        if (targetInfo.targetId === target) return p;
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
    dir, mock, main, b, pages, call, open, pageOf, wcOf, errors, log, px, secretRef, canary, attach, registeredMs, t0, writeProxyFile, homeDir,
    out: () => out,
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
    ];
    const r = await rig({ restore: (mock) => (mock.state.tabs.get('w1:t2').browser = { key: 'g-1', rev: 1, active: 'b4', entries }) });
    const { call, open, main } = r;
    const a = await open('http://app.test/');
    await titled(a.p, 'app app.test');
    check('CDP goto file: in the web session', /ERR_/.test(await goto(a.p, fileUrl)), a.p.url());
    await goto(a.p, 'http://app.test/');
    for (const u of [fileUrl, 'FILE://' + secretFile, 'view-source:' + fileUrl]) {
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
    const v = await call('show', { file: htmlFile, tab_id: 'w1:t1', opened_by: 'agent:w1:p1', focus: false });
    const vp = v.target_id && (await r.pageOf(v.target_id));
    check('show of a local file: refused and reported', vp && (await waitFor(() => r.errors().some((x) => x.text.includes(htmlFile)))) && (await vp.title()) !== 'LEAK' && !(await vp.evaluate(() => document.body.innerText)).includes('leak'), r.errors().map((x) => x.text));
    const vg = await goto(vp, 'file://' + htmlFile);
    check('CDP goto file: in the html session', /ERR_/.test(vg), vg);
    // the agents' restored pages loaded by themselves
    const rest = await waitFor(async () => {
      const x = (await call('views')).filter((y) => y.tab_id === 'w1:t2');
      return x.length === 4 && x.every((y) => y.live && y.target_id) && x;
    }, 6000);
    check('restored entries are live', rest);
    await waitFor(() => r.errors().length >= 4, 3000);
    let leaks = 0;
    for (const x of rest || []) {
      const p = await r.pageOf(x.target_id);
      const body = p ? await p.evaluate(() => document.title + document.documentElement.innerText).catch(() => '') : '';
      if (/LEAK|leak|BEGIN CERT/.test(body)) leaks++;
    }
    check('restored file: entries show nothing (web, html, html with network)', leaks === 0);
    check('... each reported', ['cert.pem', 'view.html'].every((n) => r.errors().some((x) => x.text.includes(n))), r.errors().map((x) => x.text));
    check('... and the restored web page loads', await waitFor(() => r.pages().find((p) => p.url() === 'http://app.test/tall')));
    const strip = r.pages().find((p) => p.url().startsWith('herdr-companion://ui/strip.html'));
    check('the bar is alive', strip && (await strip.evaluate(() => document.styleSheets.length > 0)));
    await r.stop('files');
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
