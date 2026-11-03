// The dock's window source: a persistent `osascript` running probe.jxa.js (macOS), or a
// fake fed from a JSON file (`HERDR_COMPANION_FAKE_WINDOWS`, test hooks only). It is the
// companion's only window-system code; other platforms have no probe (docking reports
// unsupported).
//
// `watch({pids, wid})` (re)starts the source for the windows of `pids` and the bound window
// `wid`; `stop()` ends it. Samples arrive as 'sample' events:
//   {t, target?: {num, pid, x, y, w, h, on}, above?: [[num, pid, layer]], candidates?: [...]}
// A probe that exits by itself is started again for the same windows after a pause that
// doubles while it keeps dying young (restartDelay).

'use strict';

const { EventEmitter } = require('node:events');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const PROBE = path.join(__dirname, 'probe.jxa.js');
const RATES = { interval: 100, fast: 50, every: 5 };
// The restart pause: `first`, doubled after a probe that lived less than `steady`, at most `max`.
const RESTART = { first: 1000, steady: 10000, max: 30000 };

// The pause before restarting a probe that exited after `livedMs`, `previous` the last pause
// (0: none yet).
function restartDelay(previous, livedMs) {
  if (!previous || livedMs >= RESTART.steady) return RESTART.first;
  return Math.min(RESTART.max, previous * 2);
}

class WindowProbe extends EventEmitter {
  // opts: {log, fakeFile (path) or null, platform, spawn (tests)}
  constructor({ log = () => {}, fakeFile = null, platform = process.platform, spawn: spawnFn = spawn } = {}) {
    super();
    this.log = log;
    this.fakeFile = fakeFile;
    this.platform = platform;
    this.spawn = spawnFn;
    this.child = null;
    this.timer = null;
    this.restartTimer = null;
    this.delay = 0; // the last restart pause
    this.args = null;
  }

  // Whether this platform has a window source at all.
  get available() {
    return !!this.fakeFile || this.platform === 'darwin';
  }

  watch({ pids = [], wid = 0 } = {}) {
    const key = `${pids.join(',')}|${wid || 0}`;
    // the same windows: running, or waiting to restart
    if (this.args === key && (this.child || this.timer || this.restartTimer)) return;
    this.stop();
    this.args = key;
    if (!pids.length && !wid) return;
    if (this.fakeFile) return this._fake(pids, wid);
    if (this.platform !== 'darwin') return;
    this._spawn(pids, wid);
  }

  _spawn(pids, wid) {
    const argv = ['-l', 'JavaScript', PROBE, pids.join(','), String(wid || 0), String(RATES.interval), String(RATES.fast), String(RATES.every)];
    const child = this.spawn('/usr/bin/osascript', argv, { stdio: ['ignore', 'pipe', 'pipe'] });
    this.child = child;
    const startedAt = Date.now();
    let buf = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (d) => {
      if (this.child !== child) return; // a stopped probe's last output
      buf += d;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        if (!line.trim()) continue;
        try {
          this.emit('sample', JSON.parse(line));
        } catch {}
      }
    });
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (d) => this.log('window probe:', String(d).trim().slice(0, 300)));
    // Gone by itself (an exit, or a spawn that failed): the same windows again after the pause.
    const died = (why) => {
      if (this.child !== child) return;
      this.child = null;
      this.delay = restartDelay(this.delay, Date.now() - startedAt);
      this.log(`window probe ${why}; restarting in ${this.delay / 1000} s`);
      const key = this.args;
      this.restartTimer = setTimeout(() => {
        this.restartTimer = null;
        if (this.args === key && !this.child) this._spawn(pids, wid);
      }, this.delay);
      if (this.restartTimer.unref) this.restartTimer.unref();
    };
    child.on('error', (e) => died(`failed (${e.code || e.message})`));
    child.on('exit', (code, signal) => died(`exited (${signal || code})`));
  }

  // Tests: windows from a JSON file `{windows: [{num, pid, x, y, w, h, on}]}`, front to back,
  // re-read every sample.
  _fake(pids, wid) {
    const tick = () => {
      let doc;
      try {
        doc = JSON.parse(fs.readFileSync(this.fakeFile, 'utf8'));
      } catch {
        doc = { windows: [] };
      }
      const windows = doc.windows || [];
      const sample = { t: Date.now() };
      if (wid) {
        const i = windows.findIndex((w) => w.num === wid);
        sample.target = i >= 0 ? { layer: 0, on: true, ...windows[i] } : null;
        sample.above = i >= 0 ? windows.slice(0, i).map((w) => [w.num, w.pid, w.layer || 0]) : [];
      }
      if (pids.length) sample.candidates = windows.filter((w) => (w.on !== false) && pids.includes(w.pid));
      this.emit('sample', sample);
    };
    this.timer = setInterval(tick, RATES.interval);
    tick();
  }

  stop() {
    this.args = null;
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    if (this.restartTimer) clearTimeout(this.restartTimer);
    this.restartTimer = null;
    const child = this.child;
    this.child = null;
    // The companion's own child: ended with it, never anything else.
    if (child && child.exitCode === null) child.kill('SIGTERM');
  }

  get pid() {
    return this.child ? this.child.pid : null;
  }
}

module.exports = { WindowProbe, PROBE, RATES, RESTART, restartDelay };
