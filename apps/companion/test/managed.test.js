// Managed mode (herdr's launcher): flags, register fields, the startup window rule, the quit
// signals, the socket watchdog, and the link's unregister on stop. No Electron.
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { EventEmitter } = require('node:events');
const {
  parseManaged,
  registerFields,
  SocketWatchdog,
  probeSocket,
  startupWindow,
  watchdogFor,
  onQuitSignals,
  QUIT_SIGNALS,
} = require('../src/managed.js');
const { HerdrLink } = require('../src/host.js');

const argv = (...args) => ['electron', '/app', ...args];

test('the launcher flags parse; a bad token is dropped', () => {
  const m = parseManaged(argv('--socket', '/s', '--managed', '--launch-token', 'ABCDEF0123456789', '--background'), {});
  assert.deepEqual(m, { managed: true, launchToken: 'abcdef0123456789', background: true, testHooks: false });
  assert.equal(parseManaged(argv('--launch-token=0123abcd'), {}).launchToken, '0123abcd');
  assert.equal(parseManaged(argv('--managed', '--launch-token', 'not hex!'), {}).launchToken, null);
  assert.equal(parseManaged(argv(), { HERDR_COMPANION_MANAGED: '1' }).managed, true);
  assert.equal(parseManaged(argv(), { HERDR_COMPANION_TEST_HOOKS: '1' }).testHooks, true);
  const byHand = parseManaged(argv('--socket', '/s'), {});
  assert.equal(byHand.managed, false);
  assert.equal(byHand.background, false);
});

test('the registration carries the launch token only when managed', () => {
  assert.deepEqual(registerFields(parseManaged(argv('--managed', '--launch-token', 'feedface00'), {})), {
    managed: true,
    launch_token: 'feedface00',
  });
  assert.deepEqual(registerFields(parseManaged(argv(), {})), {});
});

test('--background never shows the window; a user start shows it without the keyboard', () => {
  const m = (args) => parseManaged(argv(...args), {});
  assert.equal(startupWindow(m(['--managed', '--background']), false), 'hidden');
  assert.equal(startupWindow(m(['--managed']), false), 'inactive');
  assert.equal(startupWindow(m([]), false), 'show', 'started by hand: as before');
  assert.equal(startupWindow(m(['--managed']), true), 'hidden', 'headless never shows');
  assert.equal(startupWindow(m([]), true), 'hidden');
});

test('SIGTERM and SIGINT quit through the app (its before-quit unregisters)', () => {
  const proc = new EventEmitter();
  const quits = [];
  onQuitSignals(proc, (signal) => quits.push(signal));
  assert.deepEqual(QUIT_SIGNALS, ['SIGTERM', 'SIGINT']);
  proc.emit('SIGTERM');
  proc.emit('SIGINT');
  assert.deepEqual(quits, ['SIGTERM', 'SIGINT']);
});

function fakeClock(start = 1_000_000) {
  let t = start;
  return { now: () => t, advance: (ms) => (t += ms) };
}

test('the watchdog quits after 30 s unreachable, only when managed', async () => {
  const clock = fakeClock();
  let outcome = 'gone';
  let gone = 0;
  const dog = watchdogFor(parseManaged(argv('--managed'), {}), {
    socketPath: '/nowhere',
    probe: async () => outcome,
    now: clock.now,
    onGone: () => gone++,
  });
  assert.ok(dog instanceof SocketWatchdog);
  await dog.tick(); // first sighting
  clock.advance(29_000);
  await dog.tick();
  assert.equal(gone, 0, 'not before 30 s');
  // the socket comes back (a live handoff): the clock resets
  outcome = 'up';
  await dog.tick();
  outcome = 'gone';
  clock.advance(1000);
  await dog.tick();
  clock.advance(29_999);
  await dog.tick();
  assert.equal(gone, 0, 'a gap after a return starts over');
  // a probe that times out (a busy server) never counts as gone
  outcome = 'unknown';
  clock.advance(60_000);
  await dog.tick();
  assert.equal(gone, 0);
  outcome = 'gone';
  await dog.tick();
  clock.advance(30_000);
  await dog.tick();
  assert.equal(gone, 1, 'gone for good: quit');
  await dog.tick();
  clock.advance(60_000);
  await dog.tick();
  assert.equal(gone, 1, 'once');
  dog.stop();
  assert.equal(watchdogFor(parseManaged(argv(), {}), { socketPath: '/x', onGone: () => {} }), null, 'started by hand: no watchdog');
});

test('a socket probe tells gone from up', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hcm-'));
  const sock = path.join(dir, 's.sock');
  assert.equal(await probeSocket(sock, 500), 'gone', 'no socket file');
  const server = net.createServer((c) => c.end());
  await new Promise((r) => server.listen(sock, r));
  assert.equal(await probeSocket(sock, 500), 'up');
  await new Promise((r) => server.close(r));
  fs.rmSync(dir, { recursive: true, force: true });
});

// A herdr that answers one request per connection (like the real one) and records them.
function miniHerdr(sock) {
  const seen = [];
  const server = net.createServer((c) => {
    let buf = '';
    c.setEncoding('utf8');
    c.on('data', (d) => {
      buf += d;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        if (!line.trim()) continue;
        const req = JSON.parse(line);
        seen.push(req);
        const reply = (result) => c.write(JSON.stringify({ id: req.id, result }) + '\n');
        switch (req.method) {
          case 'browser.host_register':
            reply({ type: 'browser_host_lease', lease: { lease_id: 'L1', profile: 'companion', state: 'attached', heartbeat_ms: 60000, ttl_ms: 120000, expires_unix: 0, host_protocol: 1 } });
            c.end();
            break;
          case 'events.subscribe':
            reply({ type: 'subscription_started' }); // the stream stays open
            break;
          default:
            reply({ type: 'ok' });
            c.end();
        }
      }
    });
    c.on('error', () => {});
  });
  return { server, seen, listen: () => new Promise((r) => server.listen(sock, r)) };
}

test('the link registers with the launch token and unregisters on stop', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hcm-'));
  const sock = path.join(dir, 'h.sock');
  const herdr = miniHerdr(sock);
  await herdr.listen();
  const m = parseManaged(argv('--managed', '--launch-token', 'cafe0123'), {});
  const link = new HerdrLink({
    socketPath: sock,
    register: () => ({ kind: 'companion', instance: 'i', cdp: { host: '127.0.0.1', port: 1 }, nonce: 'n', protocol: 1, ...registerFields(m) }),
  });
  const leased = new Promise((r) => link.once('lease', r));
  link.start();
  await leased;
  const reg = herdr.seen.find((r) => r.method === 'browser.host_register');
  assert.equal(reg.params.launch_token, 'cafe0123');
  assert.equal(reg.params.managed, true);
  await link.stop();
  const unreg = herdr.seen.find((r) => r.method === 'browser.host_unregister');
  assert.ok(unreg, 'stop unregisters');
  assert.equal(unreg.params.lease_id, 'L1');
  // the hang hook: no heartbeats, no re-register
  link.hang();
  assert.equal(link.stopped, true);
  await new Promise((r) => herdr.server.close(r));
  fs.rmSync(dir, { recursive: true, force: true });
});
