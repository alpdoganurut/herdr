// The companion of another machine (`--remote <home>`; remote browser contract §4.1): every
// page request goes through the helper's authenticated proxy, whose port and secret are in
// `<home>/proxy.json`. This file holds the process switches, what every session and page gets
// (no file:, no permission, no client certificate, no DevTools), the proxy's login and the gate
// a page's first load waits for. Without `--remote`: `on` is false, `install()` and `gate()`
// do nothing and `prefs` is empty.
'use strict';
const electron = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const { cleanText } = require('./remote-rules.js');

const PROXY_HOST = 'herdr-proxy.invalid'; // never a real name: only the MAP rule below knows it
const PROXY_USER = 'herdr';
const WARM_URL = 'http://herdr-warm.invalid/'; // the proxy answers it itself: 204 with MARKER
const MARKER = 'x-herdr-proxy';
const LOGIN_TRIES = 2; // answers to the proxy's login per URL …
const LOGIN_FORGET_MS = 10000; // … until it was quiet this long (a wrong secret asks again at once)
const FAST_PROBES = 20; // a cold session's first probe can run before its proxy setting is in
const FAST_MS = 25;
const SLOW_MS = 1000; // the proxy is down: one probe a second until it is back
const PROBE_TIMEOUT_MS = 5000;
const REGISTER_WAIT_MS = 8000; // registering waits this long for the web session's first probe
const PROXY_DOWN = -130; // net::ERR_PROXY_CONNECTION_FAILED
const LABEL_MAX = 64;

function flag(argv, name) {
  const i = argv.indexOf(name);
  if (i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--')) return argv[i + 1];
  const eq = argv.find((a) => a.startsWith(name + '='));
  return eq ? eq.slice(name.length + 1) : null;
}

// `<home>/proxy.json`: {port, secret, label?}. Throws when it is missing or not usable.
function readProxyFile(home) {
  const doc = JSON.parse(fs.readFileSync(path.join(home, 'proxy.json'), 'utf8'));
  const port = doc && doc.port;
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('proxy.json: no port');
  if (typeof doc.secret !== 'string' || !/^[\x21-\x7e]{1,256}$/.test(doc.secret)) throw new Error('proxy.json: no secret');
  return { port, secret: doc.secret, label: cleanText(doc.label, LABEL_MAX) || null };
}

// The flags -> null (a local companion) or {home, config} | {home, error}. A remote companion
// never falls back to the user's own herdr or profile: both are named on its command line.
function parseRemote(argv) {
  if (!argv.some((a) => a === '--remote' || a.startsWith('--remote='))) return null;
  const home = flag(argv, '--remote');
  if (!home || !path.isAbsolute(home)) return { home: null, error: '--remote needs the absolute path of its home' };
  for (const name of ['--socket', '--profile-dir']) if (!flag(argv, name)) return { home, error: `--remote needs ${name}` };
  try {
    return { home, config: readProxyFile(home) };
  } catch (e) {
    return { home, error: `${path.join(home, 'proxy.json')}: ${e.message}` };
  }
}

const proxyRule = (port) => ({ proxyRules: `http://${PROXY_HOST}:${port}`, proxyBypassRules: '<-loopback>' });

// The process switches (before ready). A request that misses the proxy setting resolves no
// name (the MAP's NOTFOUND, 127.0.0.1 included); the port is in the MAP, or a DIRECT request
// could reach any port of this Mac under the proxy's name.
function proxySwitches(port) {
  return [
    ['disable-quic'],
    ['host-resolver-rules', `MAP ${PROXY_HOST}:${port} 127.0.0.1:${port}, MAP * ~NOTFOUND`],
    ['proxy-server', `http://${PROXY_HOST}:${port}`],
    ['proxy-bypass-list', '<-loopback>'],
  ];
}

// Every permission of the session's pages is denied: requests, checks and devices.
function denyPermissions(ses) {
  ses.setPermissionRequestHandler((_wc, _permission, cb) => cb(false));
  ses.setPermissionCheckHandler(() => false);
  ses.setDevicePermissionHandler(() => false);
}

function createRemote({ argv = process.argv, app = electron.app, net = electron.net, now = Date.now, setTimer = setTimeout, clearTimer = clearTimeout } = {}) {
  const parsed = parseRemote(argv);
  const on = !!parsed;
  const config = (parsed && parsed.config) || null;
  let log = () => {};

  // The proxy's secret as the file has it now (the helper keeps it for the companion's life;
  // a file that is gone, broken or names another port changes nothing).
  let lastSecret = config ? config.secret : null;
  function secret() {
    try {
      const doc = readProxyFile(parsed.home);
      if (doc.port === config.port) lastSecret = doc.secret;
    } catch {}
    return lastSecret;
  }

  const ours = (info) => !!info && info.isProxy === true && info.host === PROXY_HOST && info.port === config.port;

  function guardSession(ses) {
    ses.setProxy(proxyRule(config.port)).catch((e) => log('remote: proxy not set', e && e.message));
    try {
      ses.protocol.handle('file', () => Response.error());
    } catch (e) {
      log('remote: file: not refused', e && e.message);
    }
    denyPermissions(ses);
  }

  // ---------- the proxy's login ----------

  const offlineSessions = new WeakSet(); // no-network partitions: no probe, no login answer
  const tries = new Map(); // url -> {n, at}

  function mayAnswer(url) {
    const t = now();
    for (const [k, v] of tries) if (t - v.at > LOGIN_FORGET_MS) tries.delete(k);
    const n = ((tries.get(url) || {}).n || 0) + 1;
    tries.set(url, { n, at: t });
    return n <= LOGIN_TRIES;
  }

  // Only our proxy is answered; an origin's own login keeps Electron's default (cancelled).
  function onLogin(event, wc, details, info, callback) {
    if (!ours(info)) return;
    event.preventDefault();
    const ses = wc ? wc.session : null;
    if (ses && offlineSessions.has(ses)) return callback();
    if (!mayAnswer(String((details && details.url) || ''))) return callback();
    callback(PROXY_USER, secret());
    // asked although the session was warm: its cached credentials are stale. Only a probe
    // renews them for what never asks (ws, a cross-origin image).
    if (ses) lost(ses);
  }

  // ---------- the gate: a session is open once a request of its own reached our proxy ----------

  const gates = new WeakMap(); // Session -> {open, secret, error, attempts, promise, resolve}

  // One request of the session; done(null) when our proxy answered it, else done(why).
  function probe(ses, withSecret, done) {
    let settled = false;
    let timer = null;
    const finish = (why) => {
      if (settled) return;
      settled = true;
      clearTimer(timer);
      done(why);
    };
    let req;
    try {
      req = net.request({ url: WARM_URL, session: ses, cache: 'no-store', redirect: 'error' });
    } catch (e) {
      return finish(e.message);
    }
    timer = setTimer(() => {
      finish('no answer');
      req.abort();
    }, PROBE_TIMEOUT_MS);
    let asked = false;
    req.on('login', (info, cb) => {
      if (asked || !ours(info)) return cb();
      asked = true;
      cb(PROXY_USER, withSecret);
    });
    req.on('response', (res) => {
      res.on('data', () => {});
      res.on('error', () => {});
      finish(res.statusCode === 204 && res.headers[MARKER] ? null : `status ${res.statusCode}`);
    });
    req.on('error', (e) => finish(e.message));
    req.end();
  }

  function attempt(ses, g) {
    const used = secret();
    probe(ses, used, (why) => {
      if (!why) {
        log('remote: proxy answered after', g.attempts + 1, 'probes');
        Object.assign(g, { open: true, secret: used, error: null });
        return g.resolve();
      }
      g.error = why;
      g.attempts += 1;
      if (g.attempts === FAST_PROBES) log('remote: proxy not reached:', why);
      const t = setTimer(() => attempt(ses, g), g.attempts < FAST_PROBES ? FAST_MS : SLOW_MS);
      if (t && t.unref) t.unref();
    });
  }

  function shut(ses, g) {
    Object.assign(g, { open: false, secret: null, error: null, attempts: 0 });
    g.promise = new Promise((resolve) => (g.resolve = resolve));
    attempt(ses, g);
  }

  // null: the session may load now. Else a promise that resolves once it may (never rejects;
  // the probe repeats until the proxy answers).
  function gate(ses) {
    if (!on) return null;
    let g = gates.get(ses);
    if (!g) {
      g = {};
      gates.set(ses, g);
      shut(ses, g);
    } else if (g.open && g.secret !== secret()) shut(ses, g);
    return g.open ? null : g.promise;
  }

  // The proxy went away or stopped taking the session's credentials: probe again.
  function lost(ses) {
    const g = gates.get(ses);
    if (g && g.open) shut(ses, g);
  }

  // Why the session's gate is still shut.
  const why = (ses) => (gates.get(ses) || {}).error || 'no answer yet';

  // Resolves (true / false) once the session is open, or after `ms` without it.
  function warm(ses, ms = REGISTER_WAIT_MS) {
    const g = gate(ses);
    if (!g) return Promise.resolve(true);
    return new Promise((resolve) => {
      const t = setTimer(() => resolve(false), ms);
      g.then(() => {
        clearTimer(t);
        resolve(true);
      });
    });
  }

  // Before ready.
  function install(logger) {
    if (!config) return;
    if (logger) log = logger;
    for (const [name, value] of proxySwitches(config.port)) {
      if (value === undefined) app.commandLine.appendSwitch(name);
      else app.commandLine.appendSwitch(name, value);
    }
    app.on('session-created', guardSession);
    app.on('web-contents-created', (_e, wc) => {
      try {
        wc.setWebRTCIPHandlingPolicy('disable_non_proxied_udp');
      } catch (e) {
        log('remote: WebRTC policy not set', e && e.message);
      }
      // Electron ignores devTools: false for a page's own popup
      wc.on('devtools-opened', () => wc.closeDevTools());
    });
    app.on('login', onLogin);
    app.on('select-client-certificate', (event, _wc, _url, _list, callback) => {
      event.preventDefault(); // Electron's default is the first one
      callback();
    });
  }

  return {
    on,
    error: (parsed && parsed.error) || null,
    home: parsed ? parsed.home : null,
    label: config ? config.label : null,
    machine: (config && config.label) || 'remote machine', // its name on every surface
    prefs: on ? { devTools: false } : {}, // every webPreferences of the app
    install,
    gate,
    lost,
    why,
    warm,
    // a partition without network: no probe, no login answer
    offline: (ses) => {
      if (on) offlineSessions.add(ses);
    },
  };
}

module.exports = { ...createRemote(), createRemote, parseRemote, readProxyFile, proxySwitches, PROXY_HOST, PROXY_USER, WARM_URL, MARKER, PROXY_DOWN, FAST_PROBES, FAST_MS, SLOW_MS };
