// Preload of every page view, in every frame (web pages and html views; never the strip or
// the control page).
// * alert / confirm / prompt: Electron has no prompt() and shows the other two as native
//   sheets, so the three are replaced in the page's world by a synchronous call to the main
//   process, which shows the companion's own modal (src/dialogs.js) and answers. The page is
//   blocked until then, as in a browser.
// * html views without network (`--herdr-nonet`): WebRTC goes around webRequest and the
//   sidecar's routes, so its constructors are removed from the page's world first.
'use strict';
const { contextBridge, ipcRenderer } = require('electron');

const ask = (type, message, defaultValue) => ipcRenderer.sendSync('herdr-companion:dialog', { type, message, default_value: defaultValue });

contextBridge.executeInMainWorld({
  func: (ask, noNetwork) => {
    // WebIDL DOMString: undefined is the default (""), anything else is converted (a Symbol throws)
    const text = (v) => (v === undefined ? '' : `${v}`);
    const define = (name, fn) => {
      try {
        Object.defineProperty(globalThis, name, { value: fn, writable: true, enumerable: true, configurable: true });
      } catch {}
    };
    define('alert', {
      alert(message) {
        ask('alert', text(message), '');
      },
    }.alert);
    define('confirm', {
      confirm(message) {
        return ask('confirm', text(message), '') === true;
      },
    }.confirm);
    define('prompt', {
      prompt(message, defaultValue) {
        const out = ask('prompt', text(message), text(defaultValue));
        return typeof out === 'string' ? out : null;
      },
    }.prompt);
    if (!noNetwork) return;
    for (const name of ['RTCPeerConnection', 'webkitRTCPeerConnection', 'RTCDataChannel', 'RTCSessionDescription', 'RTCIceCandidate']) {
      try {
        Object.defineProperty(globalThis, name, { value: undefined, writable: false, configurable: false });
      } catch {}
    }
  },
  args: [ask, process.argv.includes('--herdr-nonet')],
});
