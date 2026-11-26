'use strict';
// The companion of another machine (src/remote.js): flags, switches, the proxy's login, the
// gate. Electron is faked; the live run is test/e2e-remote-client.mjs.
const test = require('node:test');
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const remote = require('../src/remote.js');
const { createRemote, parseRemote, proxySwitches, PROXY_HOST, PROXY_USER, WARM_URL, MARKER, FAST_PROBES, FAST_MS, SLOW_MS } = remote;

const PORT = 41234;
const dirs = [];
test.after(() => dirs.forEach((d) => fs.rmSync(d, { recursive: true, force: true })));
function home(doc = { port: PORT, secret: 's3cret' }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hc-remote-'));
  dirs.push(dir);
  if (doc) fs.writeFileSync(path.join(dir, 'proxy.json'), typeof doc === 'string' ? doc : JSON.stringify(doc), { mode: 0o600 });
  return dir;
}
const setSecret = (dir, secret, port = PORT) => fs.writeFileSync(path.join(dir, 'proxy.json'), JSON.stringify({ port, secret }));
const argvOf = (dir) => ['electron', 'app', '--socket', '/tmp/x.sock', '--profile-dir', '/tmp/p', '--remote', dir];

function fakeSession() {
  return {
    calls: [],
    setProxy(p) { this.calls.push(['proxy', p]); return Promise.resolve(); },
    protocol: { handlers: {}, handle(s, f) { this.handlers[s] = f; } },
    setPermissionRequestHandler(f) { this.request = f; },
    setPermissionCheckHandler(f) { this.check = f; },
    setDevicePermissionHandler(f) { this.device = f; },
  };
}

// A companion with a fake app, a fake net (`answer(req)` plays the proxy) and hand-run timers.
function rig({ dir = home(), answer = () => {} } = {}) {
  const app = new EventEmitter();
  app.switches = [];
  app.commandLine = { appendSwitch: (...a) => app.switches.push(a) };
  const requests = [];
  const net = {
    request(opts) {
      const req = new EventEmitter();
      req.opts = opts;
      req.abort = () => { req.aborted = true; };
      req.end = () => { requests.push(req); answer(req); };
      return req;
    },
  };
  let clock = 1000;
  let timers = [];
  const setTimer = (fn, ms) => { const t = { fn, ms, at: clock + ms }; timers.push(t); return t; };
  const clearTimer = (t) => { timers = timers.filter((x) => x !== t); };
  const logs = [];
  const r = createRemote({ argv: argvOf(dir), app, net, now: () => clock, setTimer, clearTimer });
  r.install((...a) => logs.push(a.join(' ')));
  // runs the timers due within `ms`, in order
  const advance = (ms) => {
    const end = clock + ms;
    for (;;) {
      const due = timers.filter((t) => t.at <= end).sort((a, b) => a.at - b.at)[0];
      if (!due) break;
      clearTimer(due);
      clock = due.at;
      due.fn();
    }
    clock = end;
  };
  return { r, app, requests, advance, logs, dir, timers: () => timers };
}
const respond = (req, statusCode, headers = {}) => {
  const res = new EventEmitter();
  Object.assign(res, { statusCode, headers });
  req.emit('response', res);
};
const good = (req) => respond(req, 204, { [MARKER]: 'i1' });
const down = (req) => req.emit('error', new Error('net::ERR_PROXY_CONNECTION_FAILED'));
const PROXY = { isProxy: true, host: PROXY_HOST, port: PORT };

function login(app, info, { url = 'http://a.test/', session = null } = {}) {
  const out = { prevented: false, answers: [] };
  app.emit('login', { preventDefault: () => (out.prevented = true) }, session ? { session } : null, { url }, info, (...a) => out.answers.push(a));
  return out;
}

test('without --remote: off, nothing installed, nothing read', () => {
  assert.equal(parseRemote(['electron', 'app', '--socket', '/s', '--remote-debugging-port=0', '--remote-allow-origins', '*']), null);
  const app = new EventEmitter();
  app.commandLine = { appendSwitch: () => assert.fail('a switch') };
  const r = createRemote({ argv: ['electron', 'app', '--socket', '/s'], app, net: { request: () => assert.fail('a probe') } });
  r.install(() => assert.fail('a log line'));
  assert.deepEqual(app.eventNames(), []);
  assert.equal(r.on, false);
  assert.equal(r.error, null);
  assert.equal(r.gate({}), null);
  assert.deepEqual(r.prefs, {});
  r.lost({});
  r.offline({});
  // the module itself, as main.js and views.js load it
  assert.equal(remote.on, false);
  assert.deepEqual(remote.prefs, {});
});

test('--remote needs an absolute home, a socket and a profile on the command line, and a usable proxy.json', () => {
  const dir = home();
  assert.deepEqual(parseRemote(argvOf(dir)).config, { port: PORT, secret: 's3cret', label: null });
  assert.equal(parseRemote(['e', 'a', '--socket=/s', '--profile-dir=/p', `--remote=${dir}`]).config.port, PORT);
  assert.match(parseRemote(['e', 'a', '--remote']).error, /absolute path/);
  assert.match(parseRemote(['e', 'a', '--remote', '--socket', '/s']).error, /absolute path/);
  assert.match(parseRemote(['e', 'a', '--socket', '/s', '--profile-dir', '/p', '--remote', 'rel']).error, /absolute path/);
  assert.match(parseRemote(['e', 'a', '--profile-dir', '/p', '--remote', dir]).error, /--socket/);
  assert.match(parseRemote(['e', 'a', '--socket', '/s', '--remote', dir]).error, /--profile-dir/);
  for (const bad of [null, '{', 'null', {}, { port: 0, secret: 'x' }, { port: 70000, secret: 'x' }, { port: '80', secret: 'x' }, { port: PORT }, { port: PORT, secret: '' }, { port: PORT, secret: 'a\nb' }]) {
    const got = parseRemote(argvOf(home(bad)));
    assert.ok(got.error && !got.config, JSON.stringify(bad));
    const r = createRemote({ argv: argvOf(home(bad)), app: null, net: null });
    assert.equal(r.on, true, 'never a local companion');
    assert.ok(r.error);
    assert.equal(parseRemote(argvOf(home(bad))).error.includes('s3cret'), false);
  }
  assert.equal(parseRemote(argvOf(home({ port: PORT, secret: 'x', label: 'dev\u001b[1mhost1\n' }))).config.label, 'dev[1mhost1');
});

test('a broken proxy.json is named, never quoted: its text can hold the secret', () => {
  for (const text of ['{"port":41234,"secret":leaky7}', 'secret: leaky7', 'leaky7', '{"port": 41234, "secret": "leaky7" oops}']) {
    const got = parseRemote(argvOf(home(text)));
    assert.ok(got.error && !got.config, text);
    assert.match(got.error, /proxy\.json: not valid JSON$/, text);
    assert.equal(got.error.includes('leaky7'), false, text);
  }
});

test('the switches: no EXCLUDE, the port in the MAP, no loopback bypass, no secret', () => {
  const { app, dir } = rig();
  assert.deepEqual(app.switches, proxySwitches(PORT));
  assert.deepEqual(proxySwitches(PORT), [
    ['disable-quic'],
    ['host-resolver-rules', `MAP herdr-proxy.invalid:${PORT} 127.0.0.1:${PORT}, MAP * ~NOTFOUND`],
    ['proxy-server', `http://herdr-proxy.invalid:${PORT}`],
    ['proxy-bypass-list', '<-loopback>'],
  ]);
  assert.equal(JSON.stringify(app.switches).includes('s3cret'), false);
  assert.equal(createRemote({ argv: argvOf(dir), app, net: {} }).prefs.devTools, false);
});

test('every session: the proxy, file: refused, every permission denied', async () => {
  const { app } = rig();
  const ses = fakeSession();
  app.emit('session-created', ses);
  assert.deepEqual(ses.calls, [['proxy', { proxyRules: `http://herdr-proxy.invalid:${PORT}`, proxyBypassRules: '<-loopback>' }]]);
  assert.equal(ses.protocol.handlers.file().type, 'error');
  for (const p of ['media', 'geolocation', 'notifications', 'clipboard-read', 'clipboard-sanitized-write', 'openExternal', 'fullscreen', 'fileSystem']) {
    let got = null;
    ses.request(null, p, (v) => (got = v));
    assert.equal(got, false, p);
    assert.equal(ses.check(null, p, 'https://a.test'), false, p);
  }
  assert.equal(ses.device({}), false);
});

test('every page: no non-proxied UDP, DevTools closed; no client certificate', () => {
  const { app } = rig();
  let policy = null;
  let closed = 0;
  const wc = Object.assign(new EventEmitter(), { setWebRTCIPHandlingPolicy: (p) => (policy = p), closeDevTools: () => closed++ });
  app.emit('web-contents-created', {}, wc);
  assert.equal(policy, 'disable_non_proxied_udp');
  wc.emit('devtools-opened');
  assert.equal(closed, 1);
  app.emit('web-contents-created', {}, Object.assign(new EventEmitter(), { setWebRTCIPHandlingPolicy: () => { throw new Error('gone'); } }));
  let prevented = false;
  const picked = [];
  app.emit('select-client-certificate', { preventDefault: () => (prevented = true) }, {}, 'https://a.test/', [{ subjectName: 'me' }], (...a) => picked.push(a));
  assert.equal(prevented, true);
  assert.deepEqual(picked, [[]], 'no certificate');
});

test('login: only our proxy, at most twice per URL until it is quiet again', () => {
  const { app, advance, dir } = rig();
  for (const info of [{ ...PROXY, isProxy: false }, { ...PROXY, host: '127.0.0.1' }, { ...PROXY, host: 'a.test' }, { ...PROXY, port: PORT + 1 }, null]) {
    const got = login(app, info);
    assert.deepEqual([got.prevented, got.answers], [false, []], JSON.stringify(info));
  }
  assert.deepEqual(login(app, PROXY), { prevented: true, answers: [[PROXY_USER, 's3cret']] });
  // the secret as the file has it now; a broken or moved file changes nothing
  setSecret(dir, 'newer');
  assert.deepEqual(login(app, PROXY).answers, [[PROXY_USER, 'newer']]);
  assert.deepEqual(login(app, PROXY), { prevented: true, answers: [[]] }, 'the third is cancelled');
  fs.writeFileSync(path.join(dir, 'proxy.json'), '{');
  assert.deepEqual(login(app, PROXY, { url: 'http://b.test/' }).answers, [[PROXY_USER, 'newer']], 'another URL has its own');
  setSecret(dir, 'elsewhere', PORT + 1);
  assert.deepEqual(login(app, PROXY, { url: 'http://b.test/' }).answers, [[PROXY_USER, 'newer']], 'another port is not ours');
  // a loop keeps itself capped; a quiet while forgets
  for (let i = 0; i < 20; i++) {
    advance(1000);
    assert.deepEqual(login(app, PROXY).answers, [[]]);
  }
  advance(10001);
  assert.equal(login(app, PROXY).answers[0].length, 2);
});

test('login: a no-network partition gets no answer', () => {
  const { r, app } = rig();
  const ses = fakeSession();
  r.offline(ses);
  assert.deepEqual(login(app, PROXY, { session: ses }), { prevented: true, answers: [[]] });
});

test('the gate opens on our marker only, and then costs nothing', async () => {
  const { r, requests, advance } = rig();
  const ses = fakeSession();
  const p = r.gate(ses);
  assert.ok(p instanceof Promise);
  assert.equal(r.gate(ses), p, 'one probe per session');
  assert.equal(requests.length, 1);
  assert.deepEqual(requests[0].opts, { url: WARM_URL, session: ses, cache: 'no-store', redirect: 'error' });
  // answers that are not our proxy's
  respond(requests[0], 204);
  advance(FAST_MS);
  respond(requests[1], 200, { [MARKER]: 'i1' });
  advance(FAST_MS);
  respond(requests[2], 407);
  assert.equal(r.why(ses), 'status 407');
  advance(FAST_MS);
  let open = false;
  p.then(() => (open = true));
  await null;
  assert.equal(open, false);
  good(requests[3]);
  await p;
  assert.equal(r.gate(ses), null);
  advance(60000);
  assert.equal(requests.length, 4, 'no probe while open');
  // each session has its own
  const other = fakeSession();
  assert.ok(r.gate(other) instanceof Promise);
  assert.equal(requests[4].opts.session, other);
});

test('the probe answers our proxy once with the secret, nobody else', () => {
  const { r, requests } = rig();
  r.gate(fakeSession());
  const got = [];
  const ask = (info) => requests[0].emit('login', info, (...a) => got.push(a));
  ask({ ...PROXY, isProxy: false });
  ask({ ...PROXY, port: PORT + 1 });
  ask(PROXY);
  ask(PROXY);
  assert.deepEqual(got, [[], [], [PROXY_USER, 's3cret'], []]);
});

test('the proxy down: fast probes first, then one a second, until it answers; a hung one is given up', async () => {
  let up = false;
  const h = rig({ answer: (req) => (up ? good(req) : down(req)) });
  const ses = fakeSession();
  const p = h.r.gate(ses);
  h.advance(FAST_MS * (FAST_PROBES - 1));
  assert.equal(h.requests.length, FAST_PROBES);
  assert.match(h.r.why(ses), /ERR_PROXY_CONNECTION_FAILED/);
  h.advance(SLOW_MS * 30);
  assert.equal(h.requests.length, FAST_PROBES + 30);
  assert.equal(h.logs.filter((l) => /not reached/.test(l)).length, 1, 'logged once');
  up = true;
  h.advance(SLOW_MS);
  await p;
  assert.equal(h.r.gate(ses), null);

  const hung = rig();
  hung.r.gate(fakeSession());
  hung.advance(5000);
  assert.equal(hung.requests[0].aborted, true);
  hung.advance(FAST_MS);
  assert.equal(hung.requests.length, 2);
  good(hung.requests[0]); // too late: the given-up one opens nothing
  assert.match(hung.r.why(hung.requests[0].opts.session), /no answer/);
});

test('a lost proxy, a new secret and a login of a warm session each probe again', async () => {
  const h = rig({ answer: good });
  const ses = fakeSession();
  h.r.gate(ses);
  assert.equal(h.r.gate(ses), null);
  assert.equal(h.requests.length, 1);
  h.r.lost(ses);
  assert.equal(h.requests.length, 2);
  assert.equal(h.r.gate(ses), null);
  setSecret(h.dir, 'rotated');
  assert.equal(h.r.gate(ses), null, 'open again already');
  assert.equal(h.requests.length, 3);
  const got = [];
  h.requests[2].emit('login', PROXY, (...a) => got.push(a));
  assert.deepEqual(got, [[PROXY_USER, 'rotated']]);
  login(h.app, PROXY, { session: ses });
  assert.equal(h.requests.length, 4, 'its cached credentials were stale');
  // a session that never had a gate is not probed for it
  login(h.app, PROXY, { url: 'http://c.test/', session: fakeSession() });
  h.r.lost(fakeSession());
  assert.equal(h.requests.length, 4);
});

test('warm: true once open, false after its wait', async () => {
  const h = rig({ answer: down });
  const ses = fakeSession();
  const slow = h.r.warm(ses, 8000);
  h.advance(8000);
  assert.equal(await slow, false);
  const open = h.r.warm(ses, 8000);
  const k = rig({ answer: good });
  assert.equal(await k.r.warm(fakeSession()), true);
  h.advance(8000);
  assert.equal(await open, false);
});

test('nothing logged names the secret', async () => {
  const h = rig({ answer: down });
  h.r.gate(fakeSession());
  h.advance(5000);
  login(h.app, PROXY);
  assert.ok(h.logs.length);
  assert.equal(h.logs.join('\n').includes('s3cret'), false);
});

test('every webPreferences of the app takes the remote ones (DevTools off)', () => {
  for (const [file, want] of [['main.js', 5], ['views.js', 1]]) {
    const src = fs.readFileSync(path.join(__dirname, '..', 'src', file), 'utf8');
    const blocks = src.split('webPreferences: {').slice(1).map((s) => s.slice(0, s.indexOf('\n      }')));
    assert.equal(blocks.length, want, file);
    for (const b of blocks) assert.ok(b.includes('...remote.prefs'), `${file}: ${b}`);
  }
});
