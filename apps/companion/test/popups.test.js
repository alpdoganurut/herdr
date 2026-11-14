'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { openKind, noOpener, popupWindowOptions } = require('../src/popups.js');

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
