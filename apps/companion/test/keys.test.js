'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const keys = require('../src/keys.js');

const down = (o) => ({ type: 'keyDown', control: false, alt: false, shift: false, meta: false, ...o });

test("herdr's cycle key labels parse; unknown spellings and bare keys do not", () => {
  assert.deepEqual(keys.parseLabel('ctrl+shift+b'), { ctrl: true, alt: false, shift: true, super: false, key: 'b' });
  assert.deepEqual(keys.parseLabel('super+f5'), { ctrl: false, alt: false, shift: false, super: true, key: 'f5' });
  assert.equal(keys.parseLabel('cmd+b'), null, 'herdr says super');
  assert.equal(keys.parseLabel('prefix+shift+b'), null);
  assert.equal(keys.parseLabel('b'), null, 'a bare key would take typing');
  assert.equal(keys.parseLabel('shift+b'), null);
  assert.equal(keys.parseLabel('ctrl+ctrl+b'), null);
  assert.equal(keys.parseLabel('ctrl+pagedown'), null);
  assert.equal(keys.parseLabel(''), null);
  assert.equal(keys.parseLabel(null), null);
  assert.equal(keys.parseLabels(['ctrl+shift+b', 'weird', 'alt+space']).length, 2);
  assert.deepEqual(keys.parseLabels(undefined), []);
});

test('the cycle key matches by the physical key and the exact modifiers', () => {
  const specs = keys.parseLabels(['ctrl+shift+b']);
  // macOS reports ctrl+shift+b with key "B" (or a control character); the code decides
  assert.ok(keys.isCycleKey(specs, down({ control: true, shift: true, key: 'B', code: 'KeyB' })));
  assert.ok(keys.isCycleKey(specs, down({ control: true, shift: true, key: '\u0002', code: 'KeyB' })));
  assert.ok(!keys.isCycleKey(specs, down({ control: true, key: 'b', code: 'KeyB' })), 'ctrl+b is the prefix');
  assert.ok(!keys.isCycleKey(specs, down({ control: true, shift: true, meta: true, key: 'B', code: 'KeyB' })));
  assert.ok(!keys.isCycleKey(specs, down({ control: true, shift: true, key: 'N', code: 'KeyN' })));
  assert.ok(!keys.isCycleKey(specs, { ...down({ control: true, shift: true, key: 'B', code: 'KeyB' }), type: 'keyUp' }));
  assert.ok(!keys.isCycleKey([], down({ control: true, shift: true, key: 'B', code: 'KeyB' })), 'no key from an older herdr');
  const more = keys.parseLabels(['alt+space', 'super+f5', 'ctrl+shift+tab', 'ctrl+]', 'ctrl+alt+7']);
  assert.ok(keys.isCycleKey(more, down({ alt: true, key: ' ', code: 'Space' })));
  assert.ok(keys.isCycleKey(more, down({ meta: true, key: 'F5', code: 'F5' })));
  assert.ok(keys.isCycleKey(more, down({ control: true, shift: true, key: 'Tab', code: 'Tab' })));
  assert.ok(keys.isCycleKey(more, down({ control: true, key: ']', code: 'BracketRight' })));
  assert.ok(keys.isCycleKey(more, down({ control: true, alt: true, key: '¶', code: 'Digit7' })));
});

test('Ctrl+Tab steps forward, Ctrl+Shift+Tab back; ⌘⇧] / ⌘⇧[ on macOS only', () => {
  assert.equal(keys.tabCycleStep(down({ control: true, key: 'Tab', code: 'Tab' }), 'darwin'), 1);
  assert.equal(keys.tabCycleStep(down({ control: true, shift: true, key: 'Tab', code: 'Tab' }), 'linux'), -1);
  assert.equal(keys.tabCycleStep(down({ meta: true, shift: true, key: '}', code: 'BracketRight' }), 'darwin'), 1);
  assert.equal(keys.tabCycleStep(down({ meta: true, shift: true, key: '{', code: 'BracketLeft' }), 'darwin'), -1);
  assert.equal(keys.tabCycleStep(down({ meta: true, shift: true, key: '}', code: 'BracketRight' }), 'linux'), 0);
  // ⌘] is Forward, plain Tab is the page's, alt and keyUp are not ours
  assert.equal(keys.tabCycleStep(down({ meta: true, key: ']', code: 'BracketRight' }), 'darwin'), 0);
  assert.equal(keys.tabCycleStep(down({ key: 'Tab', code: 'Tab' }), 'darwin'), 0);
  assert.equal(keys.tabCycleStep(down({ control: true, alt: true, key: 'Tab', code: 'Tab' }), 'darwin'), 0);
  assert.equal(keys.tabCycleStep({ ...down({ control: true, key: 'Tab', code: 'Tab' }), type: 'keyUp' }, 'darwin'), 0);
});

test('tab cycling wraps around both ways', () => {
  assert.equal(keys.wrapIndex(0, 1, 3), 1);
  assert.equal(keys.wrapIndex(2, 1, 3), 0);
  assert.equal(keys.wrapIndex(0, -1, 3), 2);
  assert.equal(keys.wrapIndex(1, -1, 3), 0);
  assert.equal(keys.wrapIndex(-1, 1, 3), 0, 'no active tab: the first');
  assert.equal(keys.wrapIndex(-1, -1, 3), 2, 'or the last');
  assert.equal(keys.wrapIndex(0, 1, 1), 0);
  assert.equal(keys.wrapIndex(0, 1, 0), -1);
});
