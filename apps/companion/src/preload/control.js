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
  dialogs: () => call('dialogs'), // {seq, dialogs: [{id, view_id, target_id, type, message, default_value, queued}]}
  dialog: (args) => call('dialog', args), // {view_id, id?, accept, text?}: answers the view's shown dialog
  raise: (args) => call('raise', args), // {user}: herdr's "focus companion"
  quit: () => call('quit'), // herdr's stop of a managed companion, or a headless test run; refused otherwise
  // test hooks (HERDR_COMPANION_TEST_HOOKS=1 only; unknown_op otherwise)
  __crash: () => call('__crash'),
  __hang: () => call('__hang'),
  dock_debug: () => call('dock_debug'), // the dock's inputs, decision and window bounds
  page_menu: (args) => call('page_menu', args), // the last page context menu ({close}: closes it)
  sessions_menu: () => call('sessions_menu'), // the pill menu's items as last built
  strip_preview: (args) => call('strip_preview', args), // {dock, kind, features}: the floating bar as that surface; null: its own
});
