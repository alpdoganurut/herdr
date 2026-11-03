'use strict';
// The dock's geometry model against Ghostty frames computed the way Ghostty 1.3.1 sizes its
// surface (renderer/size.zig, Surface.zig scaledPadding): Retina scale 2, cell 16x34 px.
const test = require('node:test');
const assert = require('node:assert/strict');
const { placementRect, isFullScreen, systemTitlebar, REASONS } = require('../src/dock/geometry.js');

// What Ghostty reports for a window: the frame (points, titlebar included) and the pty's
// winsize, from its padding (points), the titlebar and the cell size.
function ghostty({ x = 100, y = 200, cols, rows, padX = [2, 2], padY = [2, 2], titlebar = 28, cell = [16, 34], scale = 2, extraW = 0, extraH = 0, balance = false }) {
  const px = (pt) => Math.floor(pt * scale);
  const gridW = cols * cell[0];
  const gridH = rows * cell[1];
  // the surface: the explicit padding around the grid, plus what does not make a whole cell
  const surfaceW = gridW + px(padX[0]) + px(padX[1]) + extraW;
  const surfaceH = gridH + px(padY[0]) + px(padY[1]) + extraH;
  let extent;
  if (!balance) extent = { width: surfaceW - px(padX[0]) - px(padX[1]), height: surfaceH - px(padY[0]) - px(padY[1]) };
  else {
    const left = Math.floor((surfaceW - gridW) / 2);
    const top = Math.floor((surfaceH - gridH) / 2);
    extent = { width: surfaceW - 2 * left, height: surfaceH - 2 * top };
  }
  return {
    frame: { x, y, w: surfaceW / scale, h: surfaceH / scale + titlebar },
    placement: { grid: { cols, rows }, extent_px: extent, cell_px: { width: Math.floor(extent.width / cols), height: Math.floor(extent.height / rows) } },
    scale,
  };
}

const band = (g, x, y = 1) => ({ ...g.placement, area: { x, y, width: g.placement.grid.cols - x, height: g.placement.grid.rows - y } });

test('defaults on Retina: the band sits on its cells and runs to the window edges', () => {
  const g = ghostty({ cols: 120, rows: 36, extraW: 8, extraH: 16 });
  assert.deepEqual(g.frame, { x: 100, y: 200, w: 968, h: 652 });
  const r = placementRect({ placement: band(g, 66), frame: g.frame, scale: 2, config: null });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.model.padL, 2);
  assert.equal(r.model.titlebar, 28);
  // left: frame 100 + padding 2 + 66 cells of 8 pt; top: 200 + titlebar 28 + padding 2 + 1 row of 17 pt
  assert.deepEqual(r.rect, { x: 630, y: 247, width: 1068 - 630, height: 852 - 247 });
});

test('padding 10/6: the configured padding moves the origin', () => {
  const g = ghostty({ cols: 120, rows: 37, padX: [10, 10], padY: [6, 6], extraH: 22 });
  const r = placementRect({ placement: band(g, 60, 2), frame: g.frame, scale: 2, config: { paddingX: [10, 10], paddingY: [6, 6] } });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.model.padL, 10);
  assert.equal(r.model.titlebar, 28);
  assert.equal(r.rect.x, 100 + 10 + 60 * 8);
  assert.equal(r.rect.y, 200 + 28 + 6 + 2 * 17);
  assert.equal(r.rect.x + r.rect.width, g.frame.x + g.frame.w);
  assert.equal(r.rect.y + r.rect.height, g.frame.y + g.frame.h);
  // read with the default padding instead, the same window is refused, not misplaced
  const wrong = placementRect({ placement: band(g, 60, 2), frame: g.frame, scale: 2, config: null });
  assert.equal(wrong.ok, false);
  assert.equal(wrong.reason, REASONS.split);
});

test('balanced padding: the space beyond whole cells is split around the grid', () => {
  const g = ghostty({ cols: 120, rows: 36, extraW: 8, extraH: 16, balance: true });
  const r = placementRect({ placement: band(g, 70), frame: g.frame, scale: 2, config: { balance: true, titlebarHeight: 28 } });
  assert.equal(r.ok, true, r.reason);
  // 16 px over the grid: 8 px (4 pt) each side; 24 px: 12 px (6 pt) top
  assert.equal(r.model.padL, 4);
  assert.equal(r.model.padT, 6);
  assert.equal(r.rect.x, 100 + 4 + 70 * 8);
  assert.equal(r.rect.y, 200 + 28 + 6 + 17);
});

test('titlebar hidden and native: the titlebar is measured, not assumed', () => {
  const hidden = ghostty({ cols: 100, rows: 30, titlebar: 0, extraW: 4, extraH: 10 });
  const r = placementRect({ placement: band(hidden, 50), frame: hidden.frame, scale: 2, config: { titlebarStyle: 'hidden' } });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.model.titlebar, 0);
  assert.equal(r.rect.y, 200 + 2 + 17);
  const native = ghostty({ cols: 100, rows: 30, titlebar: 32, extraW: 4, extraH: 10 });
  const n = placementRect({ placement: band(native, 50), frame: native.frame, scale: 2, config: { titlebarStyle: 'native' } });
  assert.equal(n.model.titlebar, 32);
  assert.equal(n.rect.y, 200 + 32 + 2 + 17);
});

test('a non-Retina display: one device pixel per point', () => {
  const g = ghostty({ cols: 120, rows: 36, cell: [8, 17], scale: 1, extraW: 3, extraH: 9 });
  const r = placementRect({ placement: band(g, 66), frame: g.frame, scale: 1, config: null });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.rect.x, 100 + 2 + 66 * 8);
  assert.equal(r.rect.y, 200 + 28 + 2 + 17);
});

test('a Ghostty split (the surface is half the window) is refused with the fix', () => {
  const g = ghostty({ cols: 60, rows: 36, extraW: 8, extraH: 16 });
  const split = { ...g.frame, w: g.frame.w * 2 };
  const r = placementRect({ placement: band(g, 30), frame: split, scale: 2, config: null });
  assert.equal(r.ok, false);
  assert.equal(r.reason, REASONS.split);
});

test('no pixel size, or an area outside the grid, says why', () => {
  const g = ghostty({ cols: 120, rows: 36 });
  const none = placementRect({ placement: { ...band(g, 66), extent_px: { width: 0, height: 0 } }, frame: g.frame, scale: 2 });
  assert.equal(none.reason, REASONS.noPixels);
  const outside = placementRect({ placement: { ...band(g, 66), area: { x: 100, y: 1, width: 40, height: 10 } }, frame: g.frame, scale: 2 });
  assert.equal(outside.reason, REASONS.grid);
});

test('another terminal: evenly padded, best effort', () => {
  // 6 pt all round (iTerm2-like), titlebar 28
  const g = ghostty({ cols: 100, rows: 30, padX: [6, 6], padY: [6, 6] });
  const r = placementRect({ placement: band(g, 50), frame: g.frame, scale: 2, config: { symmetric: true } });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.model.padL, 6);
  assert.equal(r.rect.y, 200 + 28 + 6 + 17);
});

test('a frame equal to its display is full screen', () => {
  assert.equal(isFullScreen({ x: 0, y: 0, w: 1728, h: 1117 }, { x: 0, y: 0, width: 1728, height: 1117 }), true);
  assert.equal(isFullScreen({ x: 0, y: 25, w: 1728, h: 1092 }, { x: 0, y: 0, width: 1728, height: 1117 }), false);
});

test('a balanced window: the titlebar is the system one, 32 pt from macOS 26, 28 before', () => {
  assert.equal(systemTitlebar('25.4.0'), 32);
  assert.equal(systemTitlebar('24.6.0'), 28);
  // macOS 26: the same window measured with a 32 pt titlebar puts the grid 2 pt higher
  const g = ghostty({ cols: 105, rows: 31, extraW: 8, extraH: 2, balance: true, titlebar: 32 });
  const r = placementRect({ placement: band(g, 66), frame: g.frame, scale: 2, config: { balance: true, titlebarHeight: 32 } });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.model.titlebar, 32);
  assert.equal(r.model.padT, 2.5);
});
