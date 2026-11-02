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
  quit: () => call('quit'), // headless (dev/test) runs only; refused otherwise
});
