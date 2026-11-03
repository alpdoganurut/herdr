'use strict';
// The dock's core: binding a placement to its terminal window, following moves, hiding,
// z-order upkeep and what it reports to herdr (pure: fake samples, fake clock).
const test = require('node:test');
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const { DockCore, DockController, SETTLE_MS, RESETTLE_MS, NO_WINDOW, NOT_MAC, CONFIG_PENDING, CONFIG_FOLLOW_MS, geometryKey } = require('../src/dock/dock.js');
const { chooseTarget, anyDockPlacement } = require('../src/dock/placements.js');
const { REASONS } = require('../src/dock/geometry.js');

const GHOSTTY = 700;
// A 120x36 Ghostty window at (100, 200): frame 968x652, extent 1928x1240, cell 16x34.
const win = (num, x = 100, y = 200, extra = {}) => ({ num, pid: GHOSTTY, x, y, w: 968, h: 652, ...extra });
const placement = (id, extra = {}) => ({
  placement: {
    id,
    kind: 'side',
    tab_id: 'w1:t1',
    area: { x: 67, y: 1, width: 53, height: 35 },
    grid: { cols: 120, rows: 36 },
    cell_px: { width: 16, height: 34 },
    extent_px: { width: 1928, height: 1240 },
    pids: [4242, 4100, GHOSTTY],
    shown: true,
    ...extra,
  },
  focused: true,
  foreground: true,
  updated_unix: 10,
});

function core(platform = 'darwin', env = {}) {
  return new DockCore({ platform, scaleFor: () => 2, displayFor: () => ({ x: 0, y: 0, width: 2056, height: 1329 }), configFor: () => null, ...env });
}

const report = (d, id) => d.reports.find((r) => r.id === id);

test('one terminal window: bound at once, the dock covers the band', () => {
  const c = core();
  c.setPlacements([placement('p1')]);
  let d = c.decide(0);
  assert.equal(d.docked, true);
  assert.equal(report(d, 'p1').state, 'hidden'); // no windows seen yet
  assert.deepEqual(d.watch, { pids: [4242, 4100, GHOSTTY], wid: 0 });
  c.onSample({ t: 1, candidates: [win(11)] }, 1);
  d = c.decide(2);
  assert.equal(report(d, 'p1').state, 'covering');
  assert.equal(d.show, true);
  assert.equal(d.watch.wid, 11);
  // the band's left edge: 100 + 2 + 67 * 8; it runs to the window's right and bottom edges
  assert.deepEqual(d.rect, { x: 638, y: 247, width: 1068 - 638, height: 852 - 247 });
  assert.equal(d.raise, true); // not seen above the terminal yet
  c.dockWid = 99;
  c.onSample({ t: 3, target: win(11, 100, 200, { on: true }), above: [[7, 1, 20], [99, 5, 0]] }, 3);
  assert.equal(c.decide(4).raise, false); // directly above it
  c.onSample({ t: 5, target: win(11, 100, 200, { on: true }), above: [[99, 5, 0], [55, 9, 0]] }, 5);
  assert.equal(c.decide(6).raise, true, 'another window got between: back directly above');
});

test('a move hides the dock until the window settles; off screen hides it', () => {
  const c = core();
  c.setPlacements([placement('p1')]);
  c.onSample({ t: 0, candidates: [win(11)] }, 0);
  assert.equal(c.decide(1).show, true);
  assert.equal(c.decide(1).recheckAt, null, 'settled: nothing to wait for');
  c.onSample({ t: 100, target: win(11, 140, 210, { on: true }), above: [] }, 100);
  let d = c.decide(110);
  assert.equal(d.show, false);
  assert.equal(report(d, 'p1').state, 'moving');
  assert.equal(d.recheckAt, 100 + SETTLE_MS, 'decided again once it settled, sample or not');
  d = c.decide(100 + SETTLE_MS + 1);
  assert.equal(d.show, true);
  assert.equal(d.recheckAt, null);
  assert.equal(d.rect.x, 638 + 40);
  c.onSample({ t: 400, target: win(11, 140, 210, { on: false }), above: [] }, 400);
  d = c.decide(401);
  assert.equal(d.show, false);
  assert.equal(report(d, 'p1').state, 'hidden');
});

test('several same-size windows: unbound until the client focuses or the user clicks', () => {
  const c = core();
  const p = placement('p1');
  p.focused = false;
  c.setPlacements([p]);
  c.onSample({ t: 0, candidates: [win(21, 300, 100), win(22, 100, 200)] }, 0);
  let d = c.decide(1);
  assert.equal(report(d, 'p1').state, 'unbound');
  assert.equal(d.show, false);
  // the client's terminal gets the focus: the frontmost window of the next list is it
  c.setPlacements([{ ...p, focused: true }]);
  c.onSample({ t: 2, candidates: [win(21, 300, 100), win(22, 100, 200)] }, 2);
  d = c.decide(3);
  assert.equal(report(d, 'p1').state, 'covering');
  assert.equal(c.bound.wid, 21);
  // a nudge (the user clicked the placeholder): the list from before it keeps the binding
  c.setPlacements([placement('p1', { nudge: 1 })]);
  c.decide(4);
  assert.equal(c.bound.wid, 21);
  // the next list has the clicked window in front: bound to it
  c.onSample({ t: 5, candidates: [win(22, 100, 200), win(21, 300, 100)] }, 5);
  d = c.decide(6);
  assert.equal(c.bound.wid, 22);
  assert.equal(report(d, 'p1').state, 'covering');
  // answered: later lists keep it, whatever their order
  c.onSample({ t: 7, candidates: [win(21, 300, 100), win(22, 100, 200)] }, 7);
  c.decide(8);
  assert.equal(c.bound.wid, 22);
});

test('a focus flip binds from a window list newer than the flip, not the one from before the click', () => {
  const c = core();
  const p = { ...placement('p1'), focused: false };
  c.setPlacements([p]);
  // two same-size windows of the one Ghostty process; 21 is in front, herdr runs in 22
  c.onSample({ t: 0, candidates: [win(21, 300, 100), win(22, 100, 200)] }, 0);
  assert.equal(report(c.decide(1), 'p1').state, 'unbound');
  // the user clicks 22: herdr says focused before the probe's next list
  c.setPlacements([{ ...p, focused: true }]);
  let d = c.decide(2);
  assert.equal(c.bound, null, 'the list from before the click does not bind');
  assert.equal(report(d, 'p1').state, 'hidden', 'calm while it waits, not "click here"');
  c.onSample({ t: 3, target: null }, 3); // a sample without a list changes nothing
  assert.equal(c.decide(4).show, false);
  c.onSample({ t: 5, candidates: [win(22, 100, 200), win(21, 300, 100)] }, 5);
  d = c.decide(6);
  assert.equal(c.bound.wid, 22);
  assert.equal(report(d, 'p1').state, 'covering');
  // bound to 22, the focus goes and comes back (another app, then 22 again): until a newer
  // list arrives the binding stays
  c.setPlacements([{ ...p, focused: false }]);
  c.decide(7);
  c.setPlacements([{ ...p, focused: true }]);
  c.decide(8);
  assert.equal(c.bound.wid, 22);
});

test('no window of the client on screen (tmux, ssh), no pixels, not macOS: unsupported with the reason', () => {
  const c = core();
  c.setPlacements([placement('p1', { pids: [4242] })]);
  c.onSample({ t: 0, candidates: [] }, 0);
  assert.deepEqual(report(c.decide(1), 'p1'), { id: 'p1', state: 'unsupported', reason: NO_WINDOW });
  const px = core();
  px.setPlacements([placement('p2', { extent_px: { width: 0, height: 0 } })]);
  assert.equal(report(px.decide(0), 'p2').state, 'unsupported');
  const linux = core('linux');
  linux.setPlacements([placement('p3')]);
  assert.deepEqual(report(linux.decide(0), 'p3'), { id: 'p3', state: 'unsupported', reason: NOT_MAC });
});

test('a window whose size does not match the client is not taken for it', () => {
  const c = core();
  c.setPlacements([placement('p1')]);
  // the client's terminal is a split of a wider window: no fit
  c.onSample({ t: 0, candidates: [{ ...win(31), w: 1936 }] }, 0);
  const d = c.decide(1);
  assert.equal(report(d, 'p1').state, 'unsupported');
  assert.match(report(d, 'p1').reason, /Ghostty splits/);
});

test('an unshown placement keeps docked mode; another client is elsewhere', () => {
  const c = core();
  c.setPlacements([placement('p1', { shown: false })]);
  let d = c.decide(0);
  assert.equal(d.docked, true);
  assert.equal(d.show, false);
  assert.deepEqual(report(d, 'p1'), { id: 'p1', state: 'hidden' });
  const fg = placement('p1');
  const other = { ...placement('p2'), foreground: false, focused: false, updated_unix: 20 };
  c.setPlacements([fg, other]);
  c.onSample({ t: 1, candidates: [win(11)] }, 1);
  d = c.decide(2);
  assert.equal(report(d, 'p1').state, 'covering');
  assert.equal(report(d, 'p2').state, 'elsewhere');
  c.setPlacements([]);
  assert.equal(c.decide(3).docked, false);
});

test('the target: the foreground client, else a focused one, else the newest', () => {
  const a = { ...placement('a'), foreground: false, focused: false, updated_unix: 5 };
  const b = { ...placement('b'), foreground: false, focused: true, updated_unix: 1 };
  const c = { ...placement('c'), foreground: true, focused: false, updated_unix: 2 };
  assert.equal(chooseTarget([a, b, c]).placement.id, 'c');
  assert.equal(chooseTarget([a, b]).placement.id, 'b');
  assert.equal(chooseTarget([a, { ...a, placement: { ...a.placement, id: 'd' }, updated_unix: 9 }]).placement.id, 'd');
  assert.equal(chooseTarget([{ ...a, placement: { ...a.placement, shown: false } }]), null);
  assert.equal(anyDockPlacement([{ ...a, placement: { ...a.placement, shown: false } }]), true);
  assert.equal(anyDockPlacement([{ ...a, placement: { ...a.placement, kind: 'unknown' } }]), false);
});

test('full screen: a frame equal to its display', () => {
  const c = new DockCore({ platform: 'darwin', scaleFor: () => 2, displayFor: () => ({ x: 0, y: 0, width: 968, height: 652 }), configFor: () => null });
  c.setPlacements([placement('p1')]);
  c.onSample({ t: 0, candidates: [win(11, 0, 0)] }, 0);
  assert.equal(c.decide(1).fullScreen, true);
});

test('while the terminal settings are read the dock waits calmly; a split reason waits for a re-read', () => {
  let cfg = CONFIG_PENDING;
  const suspects = [];
  let rereading = false;
  const c = core('darwin', {
    configFor: () => cfg,
    configSuspect: (pid, updated) => {
      suspects.push([pid, updated]);
      return rereading;
    },
  });
  c.setPlacements([placement('p1')]);
  c.onSample({ t: 0, candidates: [win(11)] }, 0);
  let d = c.decide(1);
  assert.equal(report(d, 'p1').state, 'hidden', 'not unsupported before the settings are known');
  assert.equal(c.bound, null);
  cfg = null; // read: the defaults
  d = c.decide(2);
  assert.equal(report(d, 'p1').state, 'covering');
  // the padding changed under it (the extent no longer matches): a re-read runs -> moving
  const wider = placement('p1', { extent_px: { width: 1928 - 40, height: 1240 } });
  wider.updated_unix = 20;
  c.setPlacements([wider]);
  rereading = true;
  d = c.decide(3);
  assert.equal(report(d, 'p1').state, 'moving');
  assert.deepEqual(suspects.at(-1), [GHOSTTY, 20]);
  // nothing to re-read (or the read agrees): the reason
  rereading = false;
  d = c.decide(4);
  assert.deepEqual(report(d, 'p1'), { id: 'p1', state: 'unsupported', reason: REASONS.split });
});

test('a geometry that does not fit right after a move stays moving until RESETTLE_MS', () => {
  const c = core();
  c.setPlacements([placement('p1')]);
  c.onSample({ t: 0, candidates: [win(11)] }, 0);
  c.decide(1);
  // the window grew; herdr's placement still describes the old size
  c.onSample({ t: 100, target: win(11, 100, 200, { w: 1200, on: true }), above: [] }, 100);
  let d = c.decide(100 + SETTLE_MS + 1);
  assert.equal(report(d, 'p1').state, 'moving');
  assert.equal(d.recheckAt, 100 + RESETTLE_MS);
  d = c.decide(100 + RESETTLE_MS);
  assert.equal(report(d, 'p1').state, 'unsupported');
  assert.equal(d.recheckAt, null);
});

// The Electron side with stand-ins: a decision while the window settles arms one timer; no
// periodic decisions otherwise, none after stop().
test('the controller decides again once a moving window settled, and only then', (t) => {
  t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 1000 });
  const feed = Object.assign(new EventEmitter(), { report() {} });
  const probe = Object.assign(new EventEmitter(), { watch() {}, stop() {}, fakeFile: null });
  const host = { dockWindow: () => null, dockShowsTab() {}, enterDocked() {}, leaveDocked() {}, layout() {} };
  const electron = { screen: { getDisplayMatching: () => ({ scaleFactor: 2, bounds: { x: 0, y: 0, width: 100, height: 100 } }) }, app: {} };
  const ctl = new DockController({ host, feed, probe, headless: true, platform: 'darwin', electron });
  let decisions = 0;
  let next = { docked: true, watch: {}, reports: [], recheckAt: 1000 + SETTLE_MS };
  ctl.core = { decide: () => ((decisions += 1), next), setPlacements() {}, onSample() {}, bound: null, dockWid: null };
  ctl.apply();
  assert.equal(decisions, 1);
  next = { docked: true, watch: {}, reports: [], recheckAt: null };
  t.mock.timers.tick(SETTLE_MS - 1);
  assert.equal(decisions, 1);
  t.mock.timers.tick(1);
  assert.equal(decisions, 2, 'once it settled');
  t.mock.timers.tick(60000);
  assert.equal(decisions, 2, 'nothing periodic while it rests');
  next = { docked: true, watch: {}, reports: [], recheckAt: 70000 };
  ctl.apply();
  ctl.stop();
  t.mock.timers.tick(60000);
  assert.equal(decisions, 3, 'stop() cancels the settle timer');
  feed.emit('placements', []);
  probe.emit('sample', {});
  assert.equal(decisions, 3, 'and nothing decides after stop()');
});

// A changed vertical padding (a Ghostty config reload) changes the extent but can still give
// a valid rect, only a wrong one: the settings are read again whenever the placement's
// extent or cell size is not the one they were read for, one read per change, at most once
// a second, the old settings serving until the read lands.
test('the terminal settings are read again when the extent or cell size changes', (t) => {
  t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 10000 });
  const asked = [];
  const c = core('darwin', {
    configFor: (pid, key) => {
      asked.push([pid, key]);
      return null;
    },
  });
  c.setPlacements([placement('p1')]);
  c.onSample({ t: 0, candidates: [win(11)] }, 0);
  c.decide(1);
  assert.deepEqual(asked.at(-1), [GHOSTTY, '1928x1240/16x34'], 'the core asks with the extent and cell size');
  c.setPlacements([placement('p1', { extent_px: { width: 1928, height: 1224 } })]);
  c.decide(2);
  assert.deepEqual(asked.at(-1), [GHOSTTY, '1928x1224/16x34']);
  assert.equal(geometryKey({ extent_px: { width: 1, height: 2 }, cell_px: { width: 3, height: 4 } }), '1x2/3x4');

  // the controller: one read per change, at most every CONFIG_FOLLOW_MS
  const reads = [];
  const feed = Object.assign(new EventEmitter(), { report() {} });
  const probe = Object.assign(new EventEmitter(), { watch() {}, stop() {}, fakeFile: null });
  const host = { dockWindow: () => null, dockShowsTab() {}, enterDocked() {}, leaveDocked() {}, layout() {} };
  const electron = { screen: { getDisplayMatching: () => ({ scaleFactor: 2, bounds: { x: 0, y: 0, width: 100, height: 100 } }) }, app: {} };
  const ctl = new DockController({ host, feed, probe, headless: true, platform: 'darwin', electron, readSettings: (pid, fake, done) => reads.push(done) });
  let decisions = 0;
  ctl.apply = () => {
    decisions += 1;
  };
  assert.equal(ctl.configFor(GHOSTTY, 'A'), CONFIG_PENDING);
  assert.equal(reads.length, 1);
  ctl.configFor(GHOSTTY, 'A');
  assert.equal(reads.length, 1, 'one read at a time');
  reads[0]({ paddingY: [2, 2] });
  assert.equal(decisions, 1, 'a landed read decides again');
  assert.deepEqual(ctl.configFor(GHOSTTY, 'A').paddingY, [2, 2]);
  assert.equal(reads.length, 1, 'the extent they were read for: no read');
  // padding-y 2 -> 10, reloaded a while later: read again at once, the old ones meanwhile
  t.mock.timers.tick(5000);
  assert.deepEqual(ctl.configFor(GHOSTTY, 'B').paddingY, [2, 2]);
  assert.equal(reads.length, 2);
  ctl.configFor(GHOSTTY, 'B');
  assert.equal(reads.length, 2, 'one read per change');
  reads[1]({ paddingY: [10, 10] });
  assert.deepEqual(ctl.configFor(GHOSTTY, 'B').paddingY, [10, 10]);
  assert.equal(reads.length, 2);
  // a split reason's re-read counts for the extent of that moment (no second read after it)
  t.mock.timers.tick(CONFIG_FOLLOW_MS);
  ctl.configFor(GHOSTTY, 'C');
  assert.equal(reads.length, 3);
  ctl.readConfig(GHOSTTY);
  assert.equal(reads.length, 3, 'already reading');
  reads[2]({ paddingY: [10, 10] });
  ctl.configFor(GHOSTTY, 'C');
  assert.equal(reads.length, 3);
  // changes within a second of a read: none now, one once the second is over (it decides
  // again and asks with the extent of then)
  ctl.configFor(GHOSTTY, 'D');
  ctl.configFor(GHOSTTY, 'E');
  assert.equal(reads.length, 3);
  const before = decisions;
  t.mock.timers.tick(CONFIG_FOLLOW_MS);
  assert.equal(decisions, before + 1);
  ctl.configFor(GHOSTTY, 'E');
  assert.equal(reads.length, 4);
  reads[3]({ paddingY: [10, 10] });
  // stop() drops a waiting re-read
  ctl.configFor(GHOSTTY, 'F');
  const stopped = decisions;
  ctl.stop();
  t.mock.timers.tick(10 * CONFIG_FOLLOW_MS);
  assert.equal(decisions, stopped);
});
