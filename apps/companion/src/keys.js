// Keys the companion takes before a page sees them (main process; node --test requires it):
// herdr's browser-view cycle key (the lease's `view_cycle_keys`, herdr's `keys.cycle_browser`)
// and the tab cycling keys (Ctrl+Tab / Ctrl+Shift+Tab, ⌘⇧] / ⌘⇧[ on macOS).
'use strict';

const MODIFIERS = ['ctrl', 'alt', 'shift', 'super'];

// KeyboardEvent.code -> herdr's key name (the physical key, so shift and the layout's
// shifted symbol never change it).
const CODE_KEYS = {
  Space: 'space',
  Tab: 'tab',
  Enter: 'enter',
  NumpadEnter: 'enter',
  Escape: 'esc',
  Backspace: 'backspace',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  ArrowUp: 'up',
  ArrowDown: 'down',
  Minus: '-',
  Equal: '=',
  BracketLeft: '[',
  BracketRight: ']',
  Backslash: '\\',
  Semicolon: ';',
  Quote: "'",
  Comma: ',',
  Period: '.',
  Slash: '/',
  Backquote: '`',
};

// The key names herdr's lease uses beside single characters.
const NAMED = new Set(['space', 'tab', 'enter', 'esc', 'backspace', 'left', 'right', 'up', 'down', 'plus']);

// One `view_cycle_keys` label (`ctrl+shift+b`: modifiers ctrl, alt, shift, super, then the key)
// -> {ctrl, alt, shift, super, key}; null for anything else (a newer herdr's spelling this
// companion does not know is never guessed at).
function parseLabel(label) {
  if (typeof label !== 'string') return null;
  const parts = label.trim().toLowerCase().split('+');
  const key = parts.pop();
  if (!key) return null;
  const spec = { ctrl: false, alt: false, shift: false, super: false, key };
  for (const m of parts) {
    if (!MODIFIERS.includes(m) || spec[m]) return null;
    spec[m] = true;
  }
  if (key.length !== 1 && !NAMED.has(key) && !/^f([1-9]|1[0-9]|2[0-4])$/.test(key)) return null;
  // a bare key would take typing from every page: only chords (or a function key)
  if (!spec.ctrl && !spec.alt && !spec.super && !/^f\d+$/.test(key)) return null;
  return spec;
}

// The lease's labels, parsed (unknown ones dropped).
function parseLabels(labels) {
  return (Array.isArray(labels) ? labels : []).map(parseLabel).filter(Boolean);
}

// The herdr key name of an Electron `before-input-event` input: by its physical key first.
function inputKey(input) {
  const code = String(input.code || '');
  let m = /^Key([A-Z])$/.exec(code);
  if (m) return m[1].toLowerCase();
  m = /^(?:Digit|Numpad)([0-9])$/.exec(code);
  if (m) return m[1];
  m = /^F([0-9]{1,2})$/.exec(code);
  if (m) return `f${m[1]}`;
  if (CODE_KEYS[code]) return CODE_KEYS[code];
  return String(input.key || '').toLowerCase();
}

function matches(spec, input) {
  if (!spec || !input) return false;
  if (!!input.control !== spec.ctrl || !!input.alt !== spec.alt || !!input.shift !== spec.shift || !!input.meta !== spec.super) return false;
  const key = inputKey(input);
  if (key === spec.key) return true;
  if (spec.key === 'plus') return input.key === '+';
  // a symbol herdr names by its character (a layout without that physical key)
  return spec.key.length === 1 && String(input.key || '').toLowerCase() === spec.key;
}

// Whether a keyDown input is one of the parsed cycle keys.
function isCycleKey(specs, input) {
  if (!input || input.type !== 'keyDown') return false;
  return (specs || []).some((s) => matches(s, input));
}

// +1 (next tab), -1 (previous tab) or 0 for a keyDown input: Ctrl+Tab / Ctrl+Shift+Tab
// everywhere, ⌘⇧] / ⌘⇧[ on macOS.
function tabCycleStep(input, platform = process.platform) {
  if (!input || input.type !== 'keyDown' || input.alt) return 0;
  const code = String(input.code || '');
  const key = String(input.key || '');
  if (input.control && !input.meta && (code === 'Tab' || key === 'Tab')) return input.shift ? -1 : 1;
  if (platform === 'darwin' && input.meta && input.shift && !input.control) {
    if (code === 'BracketRight' || key === '}') return 1;
    if (code === 'BracketLeft' || key === '{') return -1;
  }
  return 0;
}

// The index `step` away from `index` in a list of `count`, wrapping around; an index not in
// the list (no active tab) starts from the first (next) or the last (previous).
function wrapIndex(index, step, count) {
  if (!(count > 0)) return -1;
  if (!(index >= 0 && index < count)) return step < 0 ? count - 1 : 0;
  return (((index + step) % count) + count) % count;
}

module.exports = { parseLabel, parseLabels, inputKey, isCycleKey, tabCycleStep, wrapIndex };
