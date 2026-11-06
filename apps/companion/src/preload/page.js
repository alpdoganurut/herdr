// Preload of every page view, in every frame (web pages and html views; never the strip or
// the control page).
// * alert / confirm / prompt: Electron has no prompt() and shows the other two as native
//   sheets, so the three are replaced in the page's world by a synchronous call to the main
//   process, which shows the companion's own modal (src/dialogs.js) and answers. The page is
//   blocked until then, as in a browser.
// * html views without network (`--herdr-nonet`): WebRTC goes around webRequest and the
//   sidecar's routes, so its constructors are removed from the page's world first.
// * page API (`--herdr-page-api`, an agent's html view shown with page_api): the top frame
//   gets a frozen, send-only window.herdr; iframes and every other page never do. The main
//   process re-checks every call (src/page-api.js), herdr again (browser.page_message).
// * no drag regions: the views sit in frameless windows, where Electron honours a page's
//   `app-region: drag` (Slack's desktop CSS) and sends the mouse over it to the window, never
//   to the page or the dock's auto-hidden bar above it. A user-origin !important rule beats
//   the page's own, in every document of every frame, before its first paint.
'use strict';
const { contextBridge, ipcRenderer, webFrame } = require('electron');

try {
  webFrame.insertCSS('*,*::before,*::after{app-region:no-drag !important;-webkit-app-region:no-drag !important}', { cssOrigin: 'user' });
} catch (e) {
  console.warn('herdr: no-drag style failed', e && e.message);
}

const ask = (type, message, defaultValue) => ipcRenderer.sendSync('herdr-companion:dialog', { type, message, default_value: defaultValue });

contextBridge.executeInMainWorld({
  func: (ask, noNetwork) => {
    // WebIDL DOMString: undefined is the default (""), anything else is converted (a Symbol
    // throws); cut here, so a huge string never crosses the sync call (dialogs.js TEXT_MAX)
    const text = (v) => (v === undefined ? '' : `${v}`.slice(0, 100000));
    // A page that is leaving gets the dialogs asked in its beforeunload, pagehide or unload
    // handlers answered at once and never shown, as Chromium does: nothing would cancel them
    // once its navigation started. Read from the event being dispatched (window.event, its
    // getter taken now), not from listeners of our own: a beforeunload listener in every frame
    // would make Chromium ask a frame blocked in a dialog before navigating it, which never
    // answers. isTrusted: an event the page dispatches itself is not a leave.
    const LEAVING = new Set(['beforeunload', 'pagehide', 'unload']);
    let eventGetter = null;
    try {
      const d = Object.getOwnPropertyDescriptor(globalThis, 'event');
      eventGetter = d && d.get ? d.get : null;
    } catch {}
    const leaving = () => {
      try {
        const e = eventGetter ? eventGetter.call(globalThis) : undefined;
        return !!(e && e.isTrusted === true && LEAVING.has(e.type));
      } catch {
        return false;
      }
    };
    const asked = (type, message, defaultValue) => (leaving() ? null : ask(type, message, defaultValue));
    // Electron assigns its own prompt() (which throws) in subframes after their preload, so each
    // starts as an accessor: an assignment of a native function (Electron's) keeps ours, the
    // page's own assignment wins; either way it is a plain data property from then on.
    const isNative = (v) => {
      try {
        return typeof v === 'function' && /\[native code\]\s*\}$/.test(Function.prototype.toString.call(v));
      } catch {
        return false;
      }
    };
    const define = (name, fn) => {
      const data = (value) => Object.defineProperty(globalThis, name, { value, writable: true, enumerable: true, configurable: true });
      try {
        Object.defineProperty(globalThis, name, {
          get: () => fn,
          set: (v) => data(isNative(v) ? fn : v),
          enumerable: true,
          configurable: true,
        });
      } catch {}
    };
    define('alert', {
      alert(message) {
        asked('alert', text(message), '');
      },
    }.alert);
    define('confirm', {
      confirm(message) {
        return asked('confirm', text(message), '') === true;
      },
    }.confirm);
    define('prompt', {
      prompt(message, defaultValue) {
        const out = asked('prompt', text(message), text(defaultValue));
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

if (process.argv.includes('--herdr-page-api') && process.isMainFrame) {
  // cut here too, so a huge payload never crosses the IPC (main checks again)
  const TEXT_MAX = 4000;
  const DATA_MAX = 32 * 1024;
  const refused = (reason) => ({ ok: false, outcome: 'refused', reason });
  let token = null; // this document's, from main (hello)
  const call = (req) => ipcRenderer.invoke('herdr-companion:page-message', req);
  const hello = async () => {
    const r = await call({ op: 'hello' });
    token = r && typeof r.token === 'string' ? r.token : null;
    return token;
  };
  const send = async (message) => {
    try {
      if (!message || typeof message !== 'object') return refused('invalid');
      const text = message.text === undefined || message.text === null ? '' : message.text;
      if (typeof text !== 'string') return refused('invalid');
      if ([...text].length > TEXT_MAX) return refused('too_large');
      let dataJson = null;
      if (message.data !== undefined) {
        try {
          dataJson = JSON.stringify(message.data);
        } catch {
          return refused('invalid');
        }
        if (dataJson !== undefined && new TextEncoder().encode(dataJson).length > DATA_MAX) return refused('too_large');
        if (dataJson === undefined) dataJson = null;
      }
      if (!text.trim() && dataJson === null) return refused('empty');
      // anti-spam, not proof of a human (a CDP click is trusted input too)
      if (!(navigator.userActivation && navigator.userActivation.isActive)) return refused('no_gesture');
      if (!token && !(await hello())) return refused('not_allowed');
      let r = await call({ op: 'send', token, text, data_json: dataJson });
      // a document restored from history holds an old token: one new hello (main checks it again)
      if (r && r.reason === 'stale_document' && (await hello())) r = await call({ op: 'send', token, text, data_json: dataJson });
      return r && typeof r === 'object' ? { ok: !!r.ok, outcome: String(r.outcome || (r.ok ? 'sent' : 'refused')) } : refused('failed');
    } catch {
      return refused('failed');
    }
  };
  contextBridge.exposeInMainWorld('herdr', Object.freeze({ send }));
}
