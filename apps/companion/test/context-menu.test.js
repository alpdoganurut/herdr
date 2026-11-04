'use strict';
// A page's context menu: what it offers for what was right-clicked (pure).
const test = require('node:test');
const assert = require('node:assert/strict');
const { pageMenuItems, pageMenuActions, downloadAllowed } = require('../src/context-menu.js');

const ids = (items) => items.map((it) => (it.type === 'separator' ? '|' : it.id));
const enabled = (items, id) => items.find((it) => it.id === id).enabled;

test('the page itself: back, forward, reload, inspect', () => {
  const items = pageMenuItems({ mediaType: 'none', editFlags: {} }, { canGoBack: true, canGoForward: false });
  assert.deepEqual(ids(items), ['back', 'forward', 'reload', '|', 'inspect']);
  assert.equal(enabled(items, 'back'), true);
  assert.equal(enabled(items, 'forward'), false);
});

test('a link: open in a new tab and copy its address; no navigation items', () => {
  const items = pageMenuItems({ linkURL: 'https://example.com/a', mediaType: 'none' }, {});
  assert.deepEqual(ids(items), ['open-link', 'copy-link', '|', 'inspect']);
  assert.equal(enabled(items, 'open-link'), true);
  // a javascript: or mailto: link is only copied
  assert.equal(enabled(pageMenuItems({ linkURL: 'mailto:a@b.c' }), 'open-link'), false);
});

test('an image (inside a link): the link section, then the image section', () => {
  const items = pageMenuItems({ linkURL: 'https://example.com/', mediaType: 'image', srcURL: 'https://example.com/i.png' });
  assert.deepEqual(ids(items), ['open-link', 'copy-link', '|', 'open-image', 'copy-image', 'copy-image-address', 'save-image', '|', 'inspect']);
  const data = pageMenuItems({ mediaType: 'image', srcURL: 'data:image/png;base64,AA' });
  assert.equal(enabled(data, 'open-image'), false, 'data: is never loaded top-level');
  assert.equal(enabled(data, 'save-image'), true);
});

test('an editable field: undo/redo and the clipboard by its edit flags', () => {
  const items = pageMenuItems({ isEditable: true, selectionText: 'hi', editFlags: { canUndo: false, canRedo: false, canCut: true, canCopy: true, canPaste: true, canSelectAll: true } });
  assert.deepEqual(ids(items), ['undo', 'redo', '|', 'cut', 'copy', 'paste', 'select-all', '|', 'inspect']);
  assert.equal(enabled(items, 'undo'), false);
  assert.equal(enabled(items, 'paste'), true);
});

test('selected text: copy', () => {
  assert.deepEqual(ids(pageMenuItems({ selectionText: ' some text ', editFlags: { canCopy: true } })), ['copy', '|', 'inspect']);
  // whitespace only is no selection
  assert.deepEqual(ids(pageMenuItems({ selectionText: '  ' })), ['back', 'forward', 'reload', '|', 'inspect']);
});

test('Save Image As… only for an image from the web or inline, never a local file', () => {
  const save = (srcURL) => enabled(pageMenuItems({ mediaType: 'image', srcURL }), 'save-image');
  assert.equal(save('https://example.com/i.png'), true);
  assert.equal(save('data:image/png;base64,AA'), true);
  assert.equal(save('file:///etc/passwd'), false);
  assert.equal(save('FILE:///Users/me/secret.png'), false);
  assert.equal(save('blob:https://example.com/1'), false);
  assert.equal(downloadAllowed('https://example.com/a.zip'), true);
  assert.equal(downloadAllowed('blob:https://example.com/1'), true);
  assert.equal(downloadAllowed('file:///etc/passwd'), false);
  assert.equal(downloadAllowed(''), false);
});

// A stand-in webContents that records what was done to it.
function fakeWc({ back = true, forward = false } = {}) {
  const calls = [];
  const wc = {
    destroyed: false,
    isDestroyed: () => wc.destroyed,
    navigationHistory: {
      canGoBack: () => back,
      canGoForward: () => forward,
      goBack: () => calls.push('goBack'),
      goForward: () => calls.push('goForward'),
    },
    reload: () => calls.push('reload'),
    downloadURL: (u) => calls.push(`download ${u}`),
    copyImageAt: (x, y) => calls.push(`copyImageAt ${x},${y}`),
    inspectElement: (x, y) => calls.push(`inspect ${x},${y}`),
  };
  for (const k of ['undo', 'redo', 'cut', 'copy', 'paste', 'selectAll']) wc[k] = () => calls.push(k);
  return { wc, calls };
}

test("every item acts on the clicked page's webContents, and reports what it did", () => {
  const { wc, calls } = fakeWc();
  const opened = [];
  const copied = [];
  const reported = [];
  const params = { linkURL: 'https://example.com/a', srcURL: 'https://example.com/i.png', x: 3, y: 4 };
  const actions = pageMenuActions({ wc, params, openTab: (u) => opened.push(u), copyText: (t) => copied.push(t), report: (a) => reported.push(a) });
  // every item the menu can offer has an action
  const all = pageMenuItems({ ...params, mediaType: 'image', isEditable: true, editFlags: {} }, {})
    .concat(pageMenuItems({}, {}))
    .filter((it) => it.id);
  for (const it of all) assert.equal(typeof actions[it.id], 'function', it.id);
  actions['open-link']();
  actions['open-image']();
  actions['copy-link']();
  actions['copy-image-address']();
  assert.deepEqual(opened, ['https://example.com/a', 'https://example.com/i.png']);
  assert.deepEqual(copied, ['https://example.com/a', 'https://example.com/i.png']);
  for (const id of ['copy-image', 'save-image', 'undo', 'redo', 'cut', 'copy', 'paste', 'select-all', 'back', 'forward', 'reload', 'inspect']) actions[id]();
  assert.deepEqual(calls, ['copyImageAt 3,4', 'download https://example.com/i.png', 'undo', 'redo', 'cut', 'copy', 'paste', 'selectAll', 'goBack', 'reload', 'inspect 3,4']);
  assert.deepEqual(reported, ['back', 'reload'], 'forward could not go: neither done nor reported');
  // the page closed while its menu was open: nothing done, nothing reported
  wc.destroyed = true;
  calls.length = 0;
  reported.length = 0;
  for (const id of ['back', 'reload', 'save-image', 'copy']) actions[id]();
  assert.deepEqual(calls, []);
  assert.deepEqual(reported, []);
  // a local file is never downloaded, even if asked
  const { wc: wc2, calls: calls2 } = fakeWc();
  pageMenuActions({ wc: wc2, params: { srcURL: 'file:///etc/passwd' }, openTab() {}, copyText() {} })['save-image']();
  assert.deepEqual(calls2, []);
});
