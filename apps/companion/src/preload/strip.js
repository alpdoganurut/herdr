// Strip/toolbar preload: a narrow bridge between the strip page and the main process.
'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('stripBridge', {
  onState: (fn) => ipcRenderer.on('strip:state', (_e, state) => fn(state)),
  onFocusUrl: (fn) => ipcRenderer.on('strip:focus-url', () => fn()),
  action: (name, args) => ipcRenderer.invoke('herdr-companion:strip', name, args === undefined ? null : args),
});
