// Popup windows of web pages (a sign-in popup: Google's "Continue with Google", Apple, MSAL):
// what a page's window.open becomes, and the window it gets. No Electron here: views.js wires
// it (setWindowOpenHandler / did-create-window).
//
// A script's window.open with window features ('popup,width=500,height=600') is a popup: it
// opens as a real child window of the companion's window, so the opener gets a live window
// handle (w.closed, w.postMessage) and the popup keeps window.opener: the flow hands its result
// back through them. Everything else a page opens (target=_blank, cmd+click, window.open
// without features, shift+click) is a tab of the same group, as before.

'use strict';

// What a page may load into a new tab or popup: never data: (opened from here it would skip
// Chromium's top-level data: block), never file: or another scheme.
const OPENABLE = /^(https?|about):/i;

// At most this many sign-in popups open at once per page (its own popups' popups included);
// a further window.open becomes a tab.
const POPUP_CAP = 2;
// A popup is never larger than this, nor than the work area of the display it opens on.
const POPUP_MAX = { width: 1280, height: 1000 };

// window.open's features string as the HTML spec tokenizes it ("tokenize the features
// argument"): name -> value, both lowercased; separators are ASCII whitespace, '=' and ','.
function tokenizeFeatures(features) {
  const s = String(features || '');
  const out = new Map();
  const ws = (c) => c === ' ' || c === '\t' || c === '\n' || c === '\f' || c === '\r';
  const sep = (c) => ws(c) || c === '=' || c === ',';
  let i = 0;
  while (i < s.length) {
    let name = '';
    let value = '';
    while (i < s.length && sep(s[i])) i++;
    while (i < s.length && !sep(s[i])) name += s[i++];
    name = name.toLowerCase();
    while (i < s.length && s[i] !== '=') {
      if (s[i] === ',' || !ws(s[i])) break;
      i++;
    }
    if (i < s.length && sep(s[i])) {
      while (i < s.length && sep(s[i])) {
        if (s[i] === ',') break;
        i++;
      }
      while (i < s.length && !sep(s[i])) value += s[i++];
      value = value.toLowerCase();
    }
    if (name) out.set(name, value);
  }
  return out;
}

// The spec's "parse a boolean feature": absent -> false; '' (bare), 'yes', 'true' -> true;
// else its leading integer (none: 0) is non-zero.
function boolFeature(map, name) {
  if (!map.has(name)) return false;
  const v = map.get(name);
  if (v === '' || v === 'yes' || v === 'true') return true;
  const m = /^[\t\n\f\r ]*([-+]?\d+)/.exec(v);
  return !!m && Number(m[1]) !== 0;
}

// The window.open features asking for no opener (noopener, or noreferrer which implies it):
// nothing to hand back, a tab is enough.
function noOpener(features) {
  const map = tokenizeFeatures(features);
  return boolFeature(map, 'noopener') || boolFeature(map, 'noreferrer');
}

// A popup's bounds kept inside `area` (its display's work area) and at most POPUP_MAX.
function clampBounds(b, area) {
  const width = Math.max(1, Math.min(b.width, area.width, POPUP_MAX.width));
  const height = Math.max(1, Math.min(b.height, area.height, POPUP_MAX.height));
  const x = Math.min(Math.max(b.x, area.x), area.x + area.width - width);
  const y = Math.min(Math.max(b.y, area.y), area.y + area.height - height);
  return { x: Math.round(x), y: Math.round(y), width: Math.round(width), height: Math.round(height) };
}

// {url, disposition, features} of Electron's window open handler -> 'popup' | 'tab' | 'deny'.
function openKind({ url, disposition, features } = {}) {
  if (!OPENABLE.test(String(url || ''))) return 'deny';
  if (disposition === 'new-window' && String(features || '').trim() && !noOpener(features)) return 'popup';
  return 'tab';
}

// The child window's options (Electron sizes it from the features first; these win). The page
// rules are the views' own: sandboxed, no Node, the page preload (the companion's dialogs,
// no drag regions), no page API bridge, no node in subframes beyond the preload. The session is
// the opener's (Electron keeps it: a popup shares its opener's process and cookies).
// The window's shape is pinned too: the page's features never make it frameless, transparent,
// unclosable, unmovable, see-through, larger than the screen or always on top (the companion
// sets the level), so a popup always looks like a window and shows its title.
function popupWindowOptions({ parent, show, preload, devTools = true }) {
  return {
    parent,
    show,
    modal: false,
    minimizable: false,
    fullscreenable: false,
    autoHideMenuBar: true,
    skipTaskbar: false,
    frame: true,
    transparent: false,
    hasShadow: true,
    movable: true,
    closable: true,
    focusable: true,
    resizable: true,
    maximizable: true,
    opacity: 1,
    titleBarStyle: 'default',
    enableLargerThanScreen: false,
    hiddenInMissionControl: false,
    alwaysOnTop: false,
    roundedCorners: true,
    acceptFirstMouse: false,
    vibrancy: undefined, // undefined replaces a feature's value: Electron's default (none)
    backgroundMaterial: undefined,
    minWidth: 0,
    minHeight: 0,
    maxWidth: 0,
    maxHeight: 0,
    webPreferences: {
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      nodeIntegrationInSubFrames: true,
      nodeIntegrationInWorker: false,
      webviewTag: false,
      webSecurity: true,
      backgroundThrottling: false,
      preload,
      additionalArguments: [],
      ...(devTools ? {} : { devTools: false }), // a remote companion's (Electron 45 ignores it here: remote.js closes them)
    },
  };
}

module.exports = { OPENABLE, POPUP_CAP, POPUP_MAX, openKind, noOpener, tokenizeFeatures, boolFeature, clampBounds, popupWindowOptions };
