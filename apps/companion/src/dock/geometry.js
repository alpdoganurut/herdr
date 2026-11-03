// Where a herdr placement is on screen (pure; launcher design §3.4).
//
// herdr's client publishes the cells it keeps for the browser (`area`), its terminal grid,
// the terminal's pixel extent (TIOCGWINSZ ws_xpixel/ws_ypixel) and cell size. The window
// probe gives the terminal window's frame (CGWindowList bounds, points, titlebar included)
// and the display's scale. Ghostty (1.3.1 source):
//   * ws_xpixel/ws_ypixel = the surface minus the explicit padding (renderer/size.zig
//     `terminal()`); padding = floor(points * scale) device px per side (Surface.zig
//     `scaledPadding`); unbalanced, the space beyond whole cells goes right and bottom.
//   * balanced (`window-padding-balance`): left = right = floor(space / 2), top the same but
//     capped at (explicit left + explicit right + cell width) / 2, the rest to the bottom.
//   * the transparent (default) and native titlebar styles have no full-size content view:
//     the surface starts below the titlebar; `hidden` has none.
// So, with s the scale, F the frame, E the extent, C the cell size:
//   pad_l = (F.w - E.w / s) / 2      (exact for symmetric x padding, balanced or not)
//   titlebar = F.h - (E.h + pad_t + pad_b) / s
//   origin = (F.x + pad_l, F.y + titlebar + pad_t)
// An area that reaches the grid's right or bottom edge runs to the window's edge (no strip
// of terminal padding beside the browser). Values are rounded to whole points (Electron's
// window bounds are integer points): at most half a point off.

'use strict';

const LIMITS = { padMax: 40, titlebarMax: 80, padTolerance: 1.01 };

const DEFAULTS = Object.freeze({ paddingX: [2, 2], paddingY: [2, 2], balance: false, titlebarStyle: 'transparent' });

const REASONS = {
  noPixels: 'This terminal does not report its pixel size',
  split: 'Let herdr fill its Ghostty window (no Ghostty splits)',
  grid: 'The browser area is outside the terminal grid',
  frame: 'The terminal window has no size yet',
};

const roundTo = (v, step) => Math.round(v / step) * step;

// A standard macOS titlebar: 32 pt from macOS 26 (Darwin 25), 28 pt before.
function systemTitlebar(release = require('node:os').release()) {
  const darwin = Number(String(release).split('.')[0]);
  return darwin >= 25 ? 32 : 28;
}

// Device px of a padding in points (Ghostty floors).
const padPx = (points, scale) => Math.floor(points * scale);

// The model's inputs from a placement and a probe sample. `config` = ghostty-config.js's
// result (or null: defaults). Returns {ok, rect?, reason?, model}.
function placementRect({ placement, frame, scale, config }) {
  const cfg = { ...DEFAULTS, ...(config || {}) };
  const s = scale > 0 ? scale : 1;
  const E = placement.extent_px || {};
  const C = placement.cell_px || {};
  const grid = placement.grid || {};
  const R = placement.area || {};
  const model = { scale: s };
  if (!frame || !(frame.w > 0) || !(frame.h > 0)) return { ok: false, reason: REASONS.frame, model };
  if (!(E.width > 0) || !(E.height > 0) || !(C.width > 0) || !(C.height > 0) || !(grid.cols > 0) || !(grid.rows > 0)) {
    return { ok: false, reason: REASONS.noPixels, model };
  }
  if (R.x + R.width > grid.cols || R.y + R.height > grid.rows || !(R.width > 0) || !(R.height > 0)) {
    return { ok: false, reason: REASONS.grid, model };
  }
  // Horizontal: the padding beside the grid, from the frame and the extent. Unbalanced it
  // must be the configured padding (else the surface is not the whole window: a Ghostty
  // split, or a config we could not read); balanced it is split evenly.
  const totalX = frame.w - E.width / s;
  model.padX = totalX;
  if (totalX < -0.5 || totalX > 2 * LIMITS.padMax) return { ok: false, reason: REASONS.split, model };
  let padL;
  if (cfg.symmetric) {
    // Another terminal: the same padding all round, best effort.
    padL = totalX / 2;
  } else if (!cfg.balance) {
    const expected = (padPx(cfg.paddingX[0], s) + padPx(cfg.paddingX[1], s)) / s;
    if (Math.abs(totalX - expected) > LIMITS.padTolerance) return { ok: false, reason: REASONS.split, model };
    padL = padPx(cfg.paddingX[0], s) / s;
  } else {
    padL = totalX / 2;
  }
  model.padL = padL;
  // Vertical: the explicit padding, or the balanced one.
  let padT;
  let padB;
  if (cfg.symmetric) {
    padT = padL;
    padB = padL;
  } else if (!cfg.balance) {
    padT = padPx(cfg.paddingY[0], s) / s;
    padB = padPx(cfg.paddingY[1], s) / s;
  } else {
    // Balanced: the extent cannot tell the titlebar from the padding, so the titlebar is
    // the style's (measured on unbalanced windows: 32 pt from macOS 26, 28 before; none
    // when hidden).
    const titlebar = cfg.titlebarStyle === 'hidden' ? 0 : cfg.titlebarHeight || systemTitlebar();
    const screenH = (frame.h - titlebar) * s;
    const space = screenH - grid.rows * C.height;
    let top = Math.max(0, Math.floor(space / 2));
    const maxTop = (padPx(cfg.paddingX[0], s) + padPx(cfg.paddingX[1], s) + C.width) / 2;
    const vshift = Math.max(0, top - maxTop);
    top -= vshift;
    padT = top / s;
    padB = (screenH - E.height) / s - padT;
  }
  const titlebar = frame.h - (E.height / s + padT + padB);
  model.padT = padT;
  model.titlebar = titlebar;
  if (titlebar < -0.5 || titlebar > LIMITS.titlebarMax) return { ok: false, reason: REASONS.split, model };
  const ox = frame.x + padL;
  const oy = frame.y + titlebar + padT;
  model.origin = { x: ox, y: oy };
  const cw = C.width / s;
  const ch = C.height / s;
  let left = ox + R.x * cw;
  let top = oy + R.y * ch;
  let right = ox + (R.x + R.width) * cw;
  let bottom = oy + (R.y + R.height) * ch;
  // Reaching the grid's edge: run to the window's edge (over the padding and the space
  // beyond whole cells).
  if (R.x + R.width >= grid.cols) right = frame.x + frame.w;
  if (R.y + R.height >= grid.rows) bottom = frame.y + frame.h;
  left = roundTo(left, 1);
  top = roundTo(top, 1);
  right = roundTo(right, 1);
  bottom = roundTo(bottom, 1);
  return { ok: true, rect: { x: left, y: top, width: right - left, height: bottom - top }, model };
}

// Whether `frame` is its display's full bounds (a native full-screen Space).
function isFullScreen(frame, display) {
  if (!frame || !display) return false;
  const near = (a, b) => Math.abs(a - b) <= 1;
  return near(frame.x, display.x) && near(frame.y, display.y) && near(frame.w, display.width) && near(frame.h, display.height);
}

module.exports = { placementRect, isFullScreen, systemTitlebar, REASONS, DEFAULTS, padPx };
