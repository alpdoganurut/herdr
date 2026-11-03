// Control page preload: exposes window.__herdrCompanion (contract §1.4) to the main world.
// Every call goes to the main process; results are JSON, errors {error:{code,message}}.
'use strict';
const { contextBridge, ipcRenderer } = require('electron');

const call = (op, args) => ipcRenderer.invoke('herdr-companion:control', op, args === undefined ? null : args);

contextBridge.exposeInMainWorld('__herdrCompanion', {
  nonce: () => call('nonce'),
  open: (args) => call('open', args),
  select: (viewId) => call('select', viewId),
  focus: (viewId) => call('focus', viewId),
  close: (viewId) => call('close', viewId),
  show: (args) => call('show', args),
  views: () => call('views'),
  raise: (args) => call('raise', args), // {user}: herdr's "focus companion"
  quit: () => call('quit'), // herdr's stop of a managed companion, or a headless test run; refused otherwise
  // test hooks (HERDR_COMPANION_TEST_HOOKS=1 only; unknown_op otherwise)
  __crash: () => call('__crash'),
  __hang: () => call('__hang'),
});
