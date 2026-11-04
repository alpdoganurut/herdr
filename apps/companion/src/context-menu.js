// A page's context menu (a right-click into a page view): what it offers for what was
// clicked, from Electron's `context-menu` params, and what each item does to the clicked
// page (pageMenuActions). No Electron here: main.js builds the menu.
//
// Sections, separated, in this order (only the ones that apply):
//   link       Open Link in New Tab, Copy Link Address
//   image      Open Image in New Tab, Copy Image, Copy Image Address, Save Image As…
//   editable   Undo, Redo | Cut, Copy, Paste, Select All (enabled by the field's edit flags)
//   selection  Copy (text selected outside an editable field)
//   page       Back, Forward, Reload (only when nothing more specific was clicked)
//   always     Inspect Element

'use strict';

const WEB = /^(https?|about):/i;
// An image the page may offer to save: from the web or inline, never a local file.
const SAVABLE_IMAGE = /^(https?|data):/i;
// What a page may download (each one through the save dialog): never a local file.
const DOWNLOADABLE = /^(https?|data|blob):/i;
const downloadAllowed = (url) => DOWNLOADABLE.test(String(url || ''));

// `params`: Electron's context-menu params; `nav`: {canGoBack, canGoForward}.
// Returns [{id, label, enabled}] with {type: 'separator'} between sections.
function pageMenuItems(params = {}, nav = {}) {
  const sections = [];
  const flags = params.editFlags || {};
  const link = String(params.linkURL || '');
  const src = String(params.srcURL || '');
  const image = params.mediaType === 'image' && !!src;
  const selected = !!String(params.selectionText || '').trim();
  if (link) {
    sections.push([
      { id: 'open-link', label: 'Open Link in New Tab', enabled: WEB.test(link) },
      { id: 'copy-link', label: 'Copy Link Address', enabled: true },
    ]);
  }
  if (image) {
    sections.push([
      { id: 'open-image', label: 'Open Image in New Tab', enabled: WEB.test(src) },
      { id: 'copy-image', label: 'Copy Image', enabled: true },
      { id: 'copy-image-address', label: 'Copy Image Address', enabled: true },
      { id: 'save-image', label: 'Save Image As…', enabled: SAVABLE_IMAGE.test(src) },
    ]);
  }
  if (params.isEditable) {
    sections.push([
      { id: 'undo', label: 'Undo', enabled: !!flags.canUndo },
      { id: 'redo', label: 'Redo', enabled: !!flags.canRedo },
    ]);
    sections.push([
      { id: 'cut', label: 'Cut', enabled: !!flags.canCut },
      { id: 'copy', label: 'Copy', enabled: !!flags.canCopy },
      { id: 'paste', label: 'Paste', enabled: !!flags.canPaste },
      { id: 'select-all', label: 'Select All', enabled: flags.canSelectAll !== false },
    ]);
  } else if (selected) {
    sections.push([{ id: 'copy', label: 'Copy', enabled: true }]);
  }
  if (!link && !image && !params.isEditable && !selected) {
    sections.push([
      { id: 'back', label: 'Back', enabled: !!nav.canGoBack },
      { id: 'forward', label: 'Forward', enabled: !!nav.canGoForward },
      { id: 'reload', label: 'Reload', enabled: true },
    ]);
  }
  sections.push([{ id: 'inspect', label: 'Inspect Element', enabled: true }]);
  const out = [];
  for (const s of sections) {
    if (out.length) out.push({ type: 'separator' });
    out.push(...s);
  }
  return out;
}

// What each item does, to the page that was right-clicked (`wc`, its webContents; the menu's
// enabled states are that page's too), never to whichever page is shown by the time the item
// is chosen. deps: {wc, params, openTab(url), copyText(text), report(action): back, forward and
// reload reported to herdr like the strip's buttons}. A page closed meanwhile: nothing.
function pageMenuActions({ wc, params = {}, openTab, copyText, report = () => {} }) {
  const link = String(params.linkURL || '');
  const src = String(params.srcURL || '');
  const live = (fn) => () => {
    if (!wc || (typeof wc.isDestroyed === 'function' && wc.isDestroyed())) return;
    fn();
  };
  return {
    'open-link': () => openTab(link),
    'copy-link': () => copyText(link),
    'open-image': () => openTab(src),
    'copy-image': live(() => wc.copyImageAt(params.x, params.y)),
    'copy-image-address': () => copyText(src),
    'save-image': live(() => SAVABLE_IMAGE.test(src) && wc.downloadURL(src)),
    undo: live(() => wc.undo()),
    redo: live(() => wc.redo()),
    cut: live(() => wc.cut()),
    copy: live(() => wc.copy()),
    paste: live(() => wc.paste()),
    'select-all': live(() => wc.selectAll()),
    back: live(() => {
      if (!wc.navigationHistory.canGoBack()) return;
      wc.navigationHistory.goBack();
      report('back');
    }),
    forward: live(() => {
      if (!wc.navigationHistory.canGoForward()) return;
      wc.navigationHistory.goForward();
      report('forward');
    }),
    reload: live(() => {
      wc.reload();
      report('reload');
    }),
    inspect: live(() => wc.inspectElement(params.x, params.y)),
  };
}

module.exports = { pageMenuItems, pageMenuActions, downloadAllowed };
