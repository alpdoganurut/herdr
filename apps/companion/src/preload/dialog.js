// The dialog card's preload (ui/dialog.html): a narrow bridge to the main process.
'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('dialogBridge', {
  onShow: (fn) => ipcRenderer.on('dialog:show', (_e, d) => fn(d)),
  onFocus: (fn) => ipcRenderer.on('dialog:focus', () => fn()),
  onSpace: (fn) => ipcRenderer.on('dialog:space', (_e, h) => fn(h)),
  ready: () => ipcRenderer.send('herdr-companion:dialog-ui', 'ready'),
  size: (height) => ipcRenderer.send('herdr-companion:dialog-ui', 'size', { height }),
  input: (id, value) => ipcRenderer.send('herdr-companion:dialog-ui', 'input', { id, value }),
  answer: (id, accept, value) => ipcRenderer.send('herdr-companion:dialog-ui', 'answer', { id, accept: !!accept, value }),
});
