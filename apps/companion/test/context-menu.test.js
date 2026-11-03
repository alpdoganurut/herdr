'use strict';
// A page's context menu: what it offers for what was right-clicked (pure).
const test = require('node:test');
const assert = require('node:assert/strict');
const { pageMenuItems } = require('../src/context-menu.js');

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
