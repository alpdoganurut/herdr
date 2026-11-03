'use strict';
// The dock's core: binding a placement to its terminal window, following moves, hiding,
// z-order upkeep and what it reports to herdr (pure: fake samples, fake clock).
const test = require('node:test');
const assert = require('node:assert/strict');
const { DockCore, SETTLE_MS, NO_WINDOW, NOT_MAC } = require('../src/dock/dock.js');
const { chooseTarget, anyDockPlacement } = require('../src/dock/placements.js');

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

function core(platform = 'darwin') {
  return new DockCore({ platform, scaleFor: () => 2, displayFor: () => ({ x: 0, y: 0, width: 2056, height: 1329 }), configFor: () => null });
}

const report = (d, id) => d.reports.find((r) => r.id === id);

test('one terminal window: bound at once, the dock covers the band', () => {
  const c = core();
  c.setPlacements([placement('p1')]);
  let d = c.decide(0);
  assert.equal(d.docked, true);
  assert.equal(report(d, 'p1').state, 'hidden'); // no windows seen yet
  assert.deepEqual(d.watch, { pids: [4242, 4100, GHOSTTY], wid: 0 });
  c.onSample({ t: 1, candidates: [win(11)], front: GHOSTTY }, 1);
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
  c.onSample({ t: 100, target: win(11, 140, 210, { on: true }), above: [] }, 100);
  let d = c.decide(110);
  assert.equal(d.show, false);
  assert.equal(report(d, 'p1').state, 'moving');
  d = c.decide(100 + SETTLE_MS + 1);
  assert.equal(d.show, true);
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
  // the client's terminal gets the focus: the frontmost window (listed first) is it
  c.setPlacements([{ ...p, focused: true }]);
  d = c.decide(2);
  assert.equal(report(d, 'p1').state, 'covering');
  assert.equal(c.bound.wid, 21);
  // a nudge (the user clicked the placeholder) binds the frontmost again
  c.onSample({ t: 3, candidates: [win(22, 100, 200), win(21, 300, 100)] }, 3);
  c.setPlacements([placement('p1', { nudge: 1 })]);
  c.decide(4);
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
