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

// The window.open features asking for no opener: nothing to hand back, a tab is enough.
function noOpener(features) {
  return String(features || '')
    .split(/[\s,]+/)
    .map((f) => f.split('=')[0].trim().toLowerCase())
    .some((f) => f === 'noopener' || f === 'noreferrer');
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
function popupWindowOptions({ parent, show, preload, backgroundColor }) {
  return {
    parent,
    show,
    modal: false,
    minimizable: false,
    fullscreenable: false,
    autoHideMenuBar: true,
    skipTaskbar: false,
    ...(backgroundColor ? { backgroundColor } : {}),
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
    },
  };
}

module.exports = { OPENABLE, openKind, noOpener, popupWindowOptions };
