// herdr JSON API client: newline JSON over the API socket (contract §0 channel A).
// One Connection per socket; host.js keeps two (requests, events stream).
'use strict';
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { EventEmitter } = require('node:events');

class ApiError extends Error {
  constructor(code, message) {
    super(message || code);
    this.code = code;
  }
}

// --socket > HERDR_SOCKET_PATH > the CLI default (session::api_socket_path_for(None)).
function resolveSocketPath(argv = process.argv, env = process.env) {
  const i = argv.indexOf('--socket');
  if (i >= 0 && argv[i + 1]) return argv[i + 1];
  const eq = argv.find((a) => a.startsWith('--socket='));
  if (eq) return eq.slice('--socket='.length);
  if (env.HERDR_SOCKET_PATH) return env.HERDR_SOCKET_PATH;
  const base = env.XDG_CONFIG_HOME ? env.XDG_CONFIG_HOME : path.join(os.homedir(), '.config');
  return path.join(base, 'herdr', 'herdr.sock');
}

// Splits a byte stream into JSON lines. Exported for tests.
class LineParser {
  constructor(onValue, onBad = () => {}) {
    this.buf = '';
    this.onValue = onValue;
    this.onBad = onBad;
  }
  push(chunk) {
    this.buf += chunk;
    let i;
    while ((i = this.buf.indexOf('\n')) >= 0) {
      const line = this.buf.slice(0, i).trim();
      this.buf = this.buf.slice(i + 1);
      if (!line) continue;
      let v;
      try {
        v = JSON.parse(line);
      } catch {
        this.onBad(line);
        continue;
      }
      this.onValue(v);
    }
  }
}

class Connection extends EventEmitter {
  constructor(socketPath, { prefix = 'c', timeoutMs = 40000 } = {}) {
    super();
    this.socketPath = socketPath;
    this.prefix = prefix;
    this.timeoutMs = timeoutMs;
    this.nextId = 1;
    this.pending = new Map();
    this.sock = null;
    this.closed = false;
  }

  // Settles exactly once: connected, a connect error (ENOENT/ECONNREFUSED: herdr is down),
  // or a timeout (a socket that never accepts).
  connect({ timeoutMs = 5000 } = {}) {
    return new Promise((resolve, reject) => {
      const sock = net.createConnection(this.socketPath);
      this.sock = sock;
      sock.setEncoding('utf8');
      let settled = false;
      const settle = (fn, v) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        fn(v);
      };
      const timer = setTimeout(() => {
        const e = new ApiError('connect_timeout', `connecting to ${this.socketPath} timed out`);
        settle(reject, e);
        sock.destroy();
      }, timeoutMs);
      const parser = new LineParser((v) => this._onValue(v));
      sock.once('connect', () => settle(resolve, this));
      sock.on('data', (d) => parser.push(d));
      sock.on('error', (e) => {
        settle(reject, e);
        this.emit('error', e);
      });
      sock.on('close', () => {
        settle(reject, new ApiError('disconnected', 'herdr connection closed before it connected'));
        this._onClose();
      });
    });
  }

  _onValue(v) {
    if (v && v.id !== undefined && this.pending.has(v.id)) {
      const p = this.pending.get(v.id);
      // events.subscribe: the first answer resolves; later lines with that id are errors.
      if (!p.stream) this.pending.delete(v.id);
      clearTimeout(p.timer);
      if (v.error) {
        if (p.stream && p.started) return this.emit('stream_error', new ApiError(v.error.code, v.error.message));
        return p.reject(new ApiError(v.error.code, v.error.message));
      }
      if (p.stream) p.started = true;
      return p.resolve(v.result);
    }
    if (v && v.event) return this.emit('event', v);
    this.emit('unmatched', v);
  }

  _onClose() {
    if (this.closed) return;
    this.closed = true;
    for (const p of this.pending.values()) {
      clearTimeout(p.timer);
      if (!p.started) p.reject(new ApiError('disconnected', 'herdr connection closed'));
    }
    this.pending.clear();
    this.emit('close');
  }

  request(method, params = {}, { stream = false, timeoutMs } = {}) {
    if (this.closed || !this.sock) return Promise.reject(new ApiError('disconnected', 'not connected'));
    const id = `${this.prefix}${this.nextId++}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new ApiError('timeout', `${method} timed out`));
      }, timeoutMs || this.timeoutMs);
      this.pending.set(id, { resolve, reject, timer, stream, started: false });
      this.sock.write(JSON.stringify({ id, method, params }) + '\n');
    });
  }

  close() {
    if (this.sock) this.sock.destroy();
    this._onClose();
  }
}

// `fn` run one call at a time, in call order: each call waits for the previous one to settle
// and resolves with its own run. A failed run is reported to `onError` and never blocks the next.
function serialized(fn, onError = () => {}) {
  let last = Promise.resolve();
  return (...args) => {
    const run = last.then(() => fn(...args));
    last = run.catch((e) => onError(e));
    return run;
  };
}

module.exports = { serialized, Connection, ApiError, LineParser, resolveSocketPath };
