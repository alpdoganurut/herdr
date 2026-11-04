// The compact bar's pure rules (the strip page loads this file; node --test requires it):
// the compact URL, the one-row / two-row breakpoint, which surface the bar is on, the
// window actions a surface offers, its pin defaults and whether the hidden bar is held open.
'use strict';
(function (root, factory) {
  const model = factory();
  if (typeof module === 'object' && module.exports) module.exports = model;
  else root.BarModel = model;
})(typeof self !== 'undefined' ? self : this, function () {
  // Below this width the bar wraps into two rows (tabs and window actions, then navigation).
  const NARROW_PX = 560;
  // The hidden bar: the hit zone at the top of the band and the hint line drawn in it.
  const ZONE_PX = 6;
  const HINT_PX = 2;
  // Hover reveal: the dwell before it shows, the grace before it hides again.
  const DWELL_MS = 180;
  const HIDE_MS = 400;
  // A navigation, a new or closed tab: the bar shows this long.
  const PULSE_MS = 1200;
  // The pin per surface: docked over herdr's band, herdr's Browser view, the floating window.
  // The floating window's bar is its drag region and holds the traffic lights: pinned.
  const PIN_DEFAULTS = Object.freeze({ docked: false, full: false, floating: true });
  const SEP = ' › '; // ›

  function decode(text) {
    try {
      return decodeURIComponent(text);
    } catch {
      return text;
    }
  }

  // `file:///…/search-it/.leap/work/x/aws-costs.html#cdn` → `search-it › aws-costs.html #cdn`:
  // the project folder (the one holding the first dot folder on the path, else the file's
  // own folder), the file, the fragment. http(s): the host (no `www.`) and the last path
  // segment, the fragment. about:blank is empty; anything else stays as it is.
  function compactUrl(raw) {
    const text = String(raw || '').trim();
    if (!text) return '';
    let u;
    try {
      u = new URL(text);
    } catch {
      return text;
    }
    const frag = u.hash && u.hash.length > 1 ? ` ${decode(u.hash)}` : '';
    if (u.protocol === 'about:') return text === 'about:blank' ? '' : text;
    if (u.protocol === 'file:') {
      const segs = decode(u.pathname).split('/').filter(Boolean);
      if (!segs.length) return text;
      const file = segs[segs.length - 1];
      const dirs = segs.slice(0, -1);
      const dot = dirs.findIndex((s) => s.startsWith('.'));
      const project = dot > 0 ? dirs[dot - 1] : dot === 0 ? '' : dirs[dirs.length - 1] || '';
      return (project ? project + SEP + file : file) + frag;
    }
    if (u.protocol === 'http:' || u.protocol === 'https:') {
      const host = u.host.replace(/^www\./, '');
      const segs = u.pathname.split('/').filter(Boolean);
      const last = segs.length ? decode(segs[segs.length - 1]) : '';
      return (last ? host + SEP + last : host) + frag;
    }
    return text;
  }

  function isNarrow(width) {
    return Number(width) > 0 && Number(width) < NARROW_PX;
  }

  // The surface the bar is on: `docked` (beside herdr's panes), `full` (herdr's Browser
  // view) or `floating` (the companion's own window).
  function surface(docked, kind) {
    if (!docked) return 'floating';
    return kind === 'full' ? 'full' : 'docked';
  }

  // The pin of `mode` in `pins` (the saved settings), else its default.
  function pinnedFor(pins, mode) {
    const saved = pins && typeof pins[mode] === 'boolean' ? pins[mode] : undefined;
    return saved === undefined ? PIN_DEFAULTS[mode] !== false : saved;
  }

  // The bar's window actions: docked, `expand` (promote) and close; in the Browser view,
  // `restore` (back to the dock) and close (back to the terminal). Only while docked, only
  // when herdr advertises `placement_actions` (an older herdr: neither button), and only when
  // the herdr client that owns the placement takes them (an older client: neither).
  function windowActions({ docked, kind, features, placementId, clientTakes }) {
    const has = features && (typeof features.has === 'function' ? features.has('placement_actions') : features.includes('placement_actions'));
    if (!docked || !has || !placementId || !clientTakes) return { expand: null, close: false };
    return { expand: kind === 'full' ? 'restore' : 'promote', close: true };
  }

  // The hidden bar stays shown while something in it needs the user: the URL field or a menu
  // is open, the note or permission bar shows, a status message shows (sent, failed, herdr
  // did not take it), a tab needs the user, there is no page.
  function held(s) {
    return !!(s.urlFocus || s.menu || s.sendbar || s.permbar || s.status || s.needsYou || s.empty);
  }

  return { NARROW_PX, ZONE_PX, HINT_PX, DWELL_MS, HIDE_MS, PULSE_MS, PIN_DEFAULTS, compactUrl, isNarrow, surface, pinnedFor, windowActions, held };
});
