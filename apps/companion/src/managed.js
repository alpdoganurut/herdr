// Managed mode: the companion herdr's launcher started (`herdr browser setup --companion`
// installs it; the server starts, watches and stops it). Pure helpers, no Electron:
// the flags, the register fields, and the socket watchdog that quits an orphan.
'use strict';
const net = require('node:net');

// The flags herdr's launcher passes (`--managed --launch-token <hex> [--background]`).
// `HERDR_COMPANION_MANAGED=1` is set in its environment too.
function parseManaged(argv = process.argv, env = process.env) {
  const value = (name) => {
    const i = argv.indexOf(name);
    if (i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--')) return argv[i + 1];
    const eq = argv.find((a) => a.startsWith(name + '='));
    return eq ? eq.slice(name.length + 1) : null;
  };
  const token = value('--launch-token');
  return {
    managed: argv.includes('--managed') || env.HERDR_COMPANION_MANAGED === '1',
    launchToken: token && /^[0-9a-f]{8,64}$/i.test(token) ? token.toLowerCase() : null,
    // started for an agent, by autostart or a crash restart: never shown, never focused
    background: argv.includes('--background'),
    // `__crash()` / `__hang()` on the control page (tests only)
    testHooks: env.HERDR_COMPANION_TEST_HOOKS === '1',
  };
}

// What `browser.host_register` gains in managed mode (JSON-only optional fields: an older
// herdr ignores them and the lease is simply unmanaged).
function registerFields(m) {
  if (!m || !m.managed) return {};
  const out = { managed: true };
  if (m.launchToken) out.launch_token = m.launchToken;
  return out;
}

// One connect to the API socket: 'up' (it accepted), 'gone' (no socket file, or nothing
// listens: ENOENT / ECONNREFUSED) or 'unknown' (anything else, e.g. a timeout while the
// server is busy: never counted as gone).
function probeSocket(socketPath, timeoutMs = 2000) {
  return new Promise((resolve) => {
    let done = false;
    const finish = (v) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      sock.destroy();
      resolve(v);
    };
    const sock = net.createConnection(socketPath);
    const timer = setTimeout(() => finish('unknown'), timeoutMs);
    sock.once('connect', () => finish('up'));
    sock.once('error', (e) => finish(e && (e.code === 'ENOENT' || e.code === 'ECONNREFUSED') ? 'gone' : 'unknown'));
  });
}

// Managed only: herdr's API socket unreachable without a break for `graceMs` (the server
// crashed or was killed; a live handoff's gap is far shorter) → `onGone()` once.
class SocketWatchdog {
  constructor({ socketPath, graceMs = 30000, everyMs = 2000, onGone, probe = probeSocket, now = Date.now, log = () => {} }) {
    this.socketPath = socketPath;
    this.graceMs = graceMs;
    this.everyMs = everyMs;
    this.onGone = onGone;
    this.probe = probe;
    this.now = now;
    this.log = log;
    this.goneSince = null;
    this.fired = false;
    this.timer = null;
    this.busy = false;
  }

  start() {
    if (this.timer) return;
    this.timer = setInterval(() => this.tick(), this.everyMs);
    if (this.timer.unref) this.timer.unref();
  }

  stop() {
    clearInterval(this.timer);
    this.timer = null;
  }

  // One probe; resolves with the outcome (tests call it directly).
  async tick() {
    if (this.busy || this.fired) return null;
    this.busy = true;
    let outcome;
    try {
      outcome = await this.probe(this.socketPath);
    } catch {
      outcome = 'unknown';
    } finally {
      this.busy = false;
    }
    const now = this.now();
    if (outcome === 'up') {
      if (this.goneSince !== null) this.log('herdr socket is back');
      this.goneSince = null;
    } else if (outcome === 'gone') {
      if (this.goneSince === null) {
        this.goneSince = now;
        this.log(`herdr socket unreachable; quitting if it stays away ${Math.round(this.graceMs / 1000)} s`);
      } else if (now - this.goneSince >= this.graceMs && !this.fired) {
        this.fired = true;
        this.stop();
        this.log('herdr socket gone for good: quitting');
        this.onGone();
      }
    }
    return outcome;
  }
}

// How the window comes up at startup: never for a background launch (an agent, autostart,
// a crash restart) or a headless run; without the keyboard for a user's managed start;
// as before (shown, focused) when started by hand.
function startupWindow(m, headless) {
  if (headless || (m && m.background)) return 'hidden';
  return m && m.managed ? 'inactive' : 'show';
}

// The watchdog for a managed companion; none when started by hand.
function watchdogFor(m, opts) {
  return m && m.managed ? new SocketWatchdog(opts) : null;
}

// SIGTERM (herdr's stop after the control page's quit) and SIGINT (a terminal's ^C) quit
// cleanly: `quit` runs the app's before-quit, which unregisters first.
const QUIT_SIGNALS = ['SIGTERM', 'SIGINT'];
function onQuitSignals(proc, quit) {
  for (const signal of QUIT_SIGNALS) proc.on(signal, () => quit(signal));
}

module.exports = {
  parseManaged,
  registerFields,
  probeSocket,
  SocketWatchdog,
  startupWindow,
  watchdogFor,
  onQuitSignals,
  QUIT_SIGNALS,
};
