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

test('the cycle key matches like herdr\'s terminal: the typed letter, exact modifiers', () => {
  const specs = keys.parseLabels(['ctrl+shift+b']);
  // the typed character decides ("B"); a control character falls back to the physical key
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
  // non-ASCII (Alt on macOS): the physical key
  assert.ok(keys.isCycleKey(more, down({ control: true, alt: true, key: '¶', code: 'Digit7' })));
});

test('letters follow the layout; symbols match shifted as herdr does', () => {
  const b = keys.parseLabels(['ctrl+shift+b']);
  // Dvorak: the physical KeyB types x, the key that types b is KeyN
  assert.ok(!keys.isCycleKey(b, down({ control: true, shift: true, key: 'X', code: 'KeyB' })), 'KeyB typing x is not b');
  assert.ok(keys.isCycleKey(b, down({ control: true, shift: true, key: 'B', code: 'KeyN' })), 'the key typing b is');
  // AZERTY: a and q swap places
  const a = keys.parseLabels(['ctrl+a']);
  assert.ok(keys.isCycleKey(a, down({ control: true, key: 'a', code: 'KeyQ' })));
  assert.ok(!keys.isCycleKey(a, down({ control: true, key: 'q', code: 'KeyA' })));
  // a non-Latin layout types no ASCII: the physical key
  assert.ok(keys.isCycleKey(b, down({ control: true, shift: true, key: 'И', code: 'KeyB' })));
  // ctrl+? is shift+/ on most layouts
  const q = keys.parseLabels(['ctrl+?']);
  assert.ok(keys.isCycleKey(q, down({ control: true, shift: true, key: '?', code: 'Slash' })));
  assert.ok(!keys.isCycleKey(q, down({ control: true, key: '/', code: 'Slash' })));
  // an explicitly shifted symbol: ctrl+shift+] types }
  const br = keys.parseLabels(['ctrl+shift+]']);
  assert.ok(keys.isCycleKey(br, down({ control: true, shift: true, key: '}', code: 'BracketRight' })));
  assert.ok(!keys.isCycleKey(br, down({ control: true, key: ']', code: 'BracketRight' })));
  // a letter needs its shift exactly: ctrl+b (the prefix) never matches shifted, nor the reverse
  assert.ok(!keys.isCycleKey(keys.parseLabels(['ctrl+alt+b']), down({ control: true, alt: true, shift: true, key: 'B', code: 'KeyB' })));
  // ctrl+plus: shift+= on US
  assert.ok(keys.isCycleKey(keys.parseLabels(['ctrl+plus']), down({ control: true, shift: true, key: '+', code: 'Equal' })));
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
