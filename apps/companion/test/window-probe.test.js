'use strict';
// The window probe's process upkeep (a stand-in spawn, fake timers) and its fake source.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { EventEmitter } = require('node:events');
const { WindowProbe, RESTART, restartDelay } = require('../src/dock/window-probe.js');

function fakeSpawn() {
  const children = [];
  const spawn = (cmd, argv) => {
    const child = new EventEmitter();
    child.argv = argv;
    child.stdout = Object.assign(new EventEmitter(), { setEncoding() {} });
    child.stderr = Object.assign(new EventEmitter(), { setEncoding() {} });
    child.exitCode = null;
    child.killed = false;
    child.kill = () => {
      child.killed = true;
    };
    child.pid = 1000 + children.length;
    children.push(child);
    return child;
  };
  return { spawn, children };
}

test('the restart pause doubles while the probe dies young, up to a cap, and resets after a steady run', () => {
  assert.equal(restartDelay(0, 10), RESTART.first);
  assert.equal(restartDelay(RESTART.first, 10), 2 * RESTART.first);
  assert.equal(restartDelay(16000, 10), RESTART.max);
  assert.equal(restartDelay(RESTART.max, 10), RESTART.max);
  assert.equal(restartDelay(16000, RESTART.steady), RESTART.first);
});

test('a probe that exits is started again for the same windows after its pause, not at the next watch', (t) => {
  t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 0 });
  const { spawn, children } = fakeSpawn();
  const probe = new WindowProbe({ platform: 'darwin', spawn });
  probe.watch({ pids: [7], wid: 3 });
  assert.equal(children.length, 1);
  children[0].emit('exit', 1, null);
  // every dock decision asks for the same windows: nothing respawns during the pause
  for (let i = 0; i < 20; i++) probe.watch({ pids: [7], wid: 3 });
  assert.equal(children.length, 1);
  t.mock.timers.tick(RESTART.first - 1);
  assert.equal(children.length, 1);
  t.mock.timers.tick(1);
  assert.equal(children.length, 2);
  assert.deepEqual(children[1].argv.slice(3, 5), ['7', '3'], 'the same windows');
  // it died young again: twice the pause
  children[1].emit('exit', 1, null);
  t.mock.timers.tick(2 * RESTART.first - 1);
  assert.equal(children.length, 2);
  t.mock.timers.tick(1);
  assert.equal(children.length, 3);
  // other windows asked for during a pause: at once
  children[2].emit('exit', 1, null);
  probe.watch({ pids: [7], wid: 4 });
  assert.equal(children.length, 4);
  // a spawn that fails is a death too, counted once (the pause keeps doubling: 8 s now)
  children[3].emit('error', Object.assign(new Error('spawn'), { code: 'EAGAIN' }));
  children[3].emit('exit', null, null);
  t.mock.timers.tick(8 * RESTART.first - 1);
  assert.equal(children.length, 4);
  t.mock.timers.tick(1);
  assert.equal(children.length, 5);
  // stop() cancels a pending restart
  children[4].emit('exit', 1, null);
  probe.stop();
  t.mock.timers.tick(10 * RESTART.max);
  assert.equal(children.length, 5);
});

test("a stopped probe's last output is not a sample", () => {
  const { spawn, children } = fakeSpawn();
  const probe = new WindowProbe({ platform: 'darwin', spawn });
  const samples = [];
  probe.on('sample', (s) => samples.push(s));
  probe.watch({ pids: [7], wid: 0 });
  children[0].stdout.emit('data', '{"t":1}\n');
  probe.stop();
  assert.equal(children[0].killed, true);
  children[0].stdout.emit('data', '{"t":2}\n');
  assert.deepEqual(samples, [{ t: 1 }]);
});

test("the fake source's samples carry the windows only", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'herdr-probe-'));
  const file = path.join(dir, 'windows.json');
  fs.writeFileSync(file, JSON.stringify({ windows: [{ num: 5, pid: 7, x: 0, y: 0, w: 10, h: 10, on: true }], front: 7 }));
  const probe = new WindowProbe({ fakeFile: file });
  const samples = [];
  probe.on('sample', (s) => samples.push(s));
  probe.watch({ pids: [7], wid: 5 });
  probe.stop();
  fs.rmSync(dir, { recursive: true, force: true });
  assert.equal(samples.length, 1);
  assert.equal(samples[0].target.num, 5);
  assert.equal(samples[0].candidates.length, 1);
  assert.equal('front' in samples[0], false);
});
