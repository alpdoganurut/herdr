'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { openKind, noOpener, popupWindowOptions, clampBounds, tokenizeFeatures, POPUP_MAX, POPUP_CAP } = require('../src/popups.js');

test('a script window.open with features is a popup', () => {
  assert.strictEqual(openKind({ url: 'https://accounts.google.com/o/oauth2/v2/auth?x=1', disposition: 'new-window', features: 'popup,width=500,height=600' }), 'popup');
  assert.strictEqual(openKind({ url: 'about:blank', disposition: 'new-window', features: 'width=500,height=600' }), 'popup');
});

test('links, cmd+click, shift+click and window.open without features stay tabs', () => {
  assert.strictEqual(openKind({ url: 'https://a.test/', disposition: 'foreground-tab', features: '' }), 'tab');
  assert.strictEqual(openKind({ url: 'https://a.test/', disposition: 'background-tab', features: '' }), 'tab');
  assert.strictEqual(openKind({ url: 'https://a.test/', disposition: 'new-window', features: '' }), 'tab');
  assert.strictEqual(openKind({ url: 'https://a.test/', disposition: 'other', features: 'width=1' }), 'tab');
});

test('a popup that asks for no opener is a tab', () => {
  assert.strictEqual(openKind({ url: 'https://a.test/', disposition: 'new-window', features: 'width=500, noopener' }), 'tab');
  assert.strictEqual(openKind({ url: 'https://a.test/', disposition: 'new-window', features: 'noreferrer=1,width=5' }), 'tab');
  assert.ok(!noOpener('popup,width=500'));
});

test('data:, file: and other schemes never open', () => {
  for (const url of ['data:text/html,<b>x</b>', 'file:///etc/passwd', 'javascript:alert(1)', 'chrome://settings', '']) {
    assert.strictEqual(openKind({ url, disposition: 'new-window', features: 'width=5' }), 'deny', url);
    assert.strictEqual(openKind({ url, disposition: 'foreground-tab', features: '' }), 'deny', url);
  }
});

test('the popup window keeps the page rules: sandboxed, no node, the page preload, no page API', () => {
  const o = popupWindowOptions({ parent: 'P', show: false, preload: '/x/page.js' });
  assert.strictEqual(o.parent, 'P');
  assert.strictEqual(o.show, false);
  const w = o.webPreferences;
  assert.strictEqual(w.sandbox, true);
  assert.strictEqual(w.contextIsolation, true);
  assert.strictEqual(w.nodeIntegration, false);
  assert.strictEqual(w.webviewTag, false);
  assert.strictEqual(w.preload, '/x/page.js');
  assert.deepStrictEqual(w.additionalArguments, []);
  assert.ok(!('session' in w) && !('partition' in w), 'the opener keeps its session');
  assert.ok(!('width' in o) && !('height' in o), 'the features size it');
});

test('noopener / noreferrer values parse as the spec says', () => {
  for (const f of ['noopener', 'noopener=yes', 'noopener=true', 'noopener=1', 'noopener=2', 'noopener = 3', 'noopener=-1', 'NOOPENER', 'noreferrer', 'width=5,noreferrer=1'])
    assert.strictEqual(noOpener(f), true, f);
  for (const f of ['noopener=0', 'noopener=no', 'noopener=false', 'noopener=abc', 'noreferrer=0', 'popup,width=500', '', 'xnoopener'])
    assert.strictEqual(noOpener(f), false, f);
  // a popup asking noopener=0 keeps its opener: a popup
  assert.strictEqual(openKind({ url: 'https://a.test/', disposition: 'new-window', features: 'width=500,noopener=0' }), 'popup');
  assert.deepStrictEqual([...tokenizeFeatures(' Width = 500 , popup,,height=6')], [['width', '500'], ['popup', ''], ['height', '6']]);
});

test("the page's features never shape the window: frame, transparency, closing, size limits, level", () => {
  const o = popupWindowOptions({ parent: 'P', show: true, preload: '/p.js' });
  const pins = {
    frame: true, transparent: false, hasShadow: true, movable: true, closable: true, focusable: true, resizable: true, maximizable: true,
    opacity: 1, titleBarStyle: 'default', enableLargerThanScreen: false, hiddenInMissionControl: false, alwaysOnTop: false,
    roundedCorners: true, acceptFirstMouse: false, skipTaskbar: false, minWidth: 0, minHeight: 0, maxWidth: 0, maxHeight: 0,
  };
  for (const [k, v] of Object.entries(pins)) assert.strictEqual(o[k], v, k);
  // present as undefined: they replace a feature's value when Electron merges the options
  for (const k of ['vibrancy', 'backgroundMaterial']) assert.ok(k in o && o[k] === undefined, k);
  assert.ok(!('backgroundColor' in o));
});

test('a popup is kept inside the work area and at most POPUP_MAX', () => {
  const area = { x: 0, y: 25, width: 1440, height: 875 };
  assert.deepStrictEqual(clampBounds({ x: 100, y: 100, width: 500, height: 600 }, area), { x: 100, y: 100, width: 500, height: 600 });
  const big = clampBounds({ x: -500, y: -500, width: 6000, height: 6000 }, area);
  assert.deepStrictEqual(big, { x: 0, y: 25, width: Math.min(1440, POPUP_MAX.width), height: Math.min(875, POPUP_MAX.height) });
  const off = clampBounds({ x: 5000, y: 5000, width: 400, height: 300 }, area);
  assert.deepStrictEqual(off, { x: 1040, y: 600, width: 400, height: 300 });
  assert.strictEqual(POPUP_CAP, 2);
});

test("a remote companion's popups have no DevTools; a local one's are as before", () => {
  const prefs = (o) => popupWindowOptions({ parent: 'P', show: false, preload: '/p.js', ...o }).webPreferences;
  assert.ok(!('devTools' in prefs({})));
  assert.deepStrictEqual(prefs({ devTools: true }), prefs({}));
  assert.deepStrictEqual(prefs({ devTools: false }), { ...prefs({}), devTools: false });
});
