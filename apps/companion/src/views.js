// One WebContentsView per page (contract §6). Web pages share the persistent profile
// partition; every html view gets its own in-memory sandbox partition (§4.3).
'use strict';
const { WebContentsView, session, net, app } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { pathToFileURL, fileURLToPath } = require('node:url');

const WEB_PARTITION = 'persist:herdr-companion';
const NETWORK_URLS = ['http://*/*', 'https://*/*', 'ws://*/*', 'wss://*/*'];
// every page view, every frame: the companion's alert/confirm/prompt (src/dialogs.js) and, in
// html views without network, no WebRTC
const PAGE_PRELOAD = path.join(__dirname, 'preload', 'page.js');
const { downloadAllowed } = require('./context-menu.js');
const { openKind, popupWindowOptions } = require('./popups.js');

// A page session's user agent without the Electron/x.y.z token: sites that see it serve their
// desktop-app pages (Slack: window drag regions, app-only behaviour) instead of the web ones.
function webUserAgent(ses, log) {
  try {
    ses.setUserAgent(ses.getUserAgent().replace(/\sElectron\/\S+/, ''));
  } catch (e) {
    log('page user agent not set:', e && e.message);
  }
}

// Every download of a page's session (a link, Save Image As…) goes through the save dialog
// (no save path is set: Electron asks where), starting in Downloads; a local file is never
// copied out (file: and other non-web URLs are cancelled).
function guardDownloads(ses) {
  ses.on('will-download', (_e, item) => {
    if (!downloadAllowed(item.getURL())) return item.cancel();
    try {
      item.setSaveDialogOptions({ defaultPath: path.join(app.getPath('downloads'), item.getFilename() || 'download') });
    } catch {}
  });
}

function within(root, p) {
  return p === root || p.startsWith(root.endsWith(path.sep) ? root : root + path.sep);
}

// Web page permissions: never auto-granted (Electron's default grants everything).
// These four are asked in the strip (allow once / deny); everything else is denied.
const PROMPTED = new Set(['geolocation', 'notifications', 'media', 'clipboard-read']);
// Granted without asking, as Chromium does for a page the user is on.
const ALLOWED = new Set(['clipboard-sanitized-write']);

function originOf(url) {
  try {
    return new URL(url).origin;
  } catch {
    return '';
  }
}

// The directory an html view may read: `root` resolved, else (the file is not inside it,
// e.g. a symlink to another folder) the directory the file really lives in.
function viewRoot(root, file) {
  const real = (p) => {
    try {
      return fs.realpathSync(p);
    } catch {
      return p ? path.resolve(p) : p;
    }
  };
  const fileReal = file ? real(file) : null;
  const rootReal = real(root || (file ? path.dirname(file) : '/'));
  if (fileReal && !within(rootReal, fileReal)) return path.dirname(fileReal);
  return rootReal;
}

// The partition of html views reading `rootReal` with or without network. Electron never
// frees a session, so views of the same directory and policy share one (in memory).
function viewPartition(rootReal, allowNetwork) {
  const h = crypto.createHash('sha256').update(rootReal).digest('hex').slice(0, 16);
  return `herdr-view-${h}-${allowNetwork ? 'net' : 'nonet'}`;
}

// A web proxy nothing answers: what still tries the network (WebRTC over TCP) gets nowhere.
const DEAD_PROXY = { proxyRules: 'http://127.0.0.1:9', proxyBypassRules: '<-loopback>' };

// html_view sandbox: file:// limited to `rootReal` (canonicalized, no symlink escape),
// network blocked unless allowed, every permission denied. Set up once per partition.
const htmlSessions = new Map(); // partition -> {ses, rootReal, ready}
function htmlSession(rootReal, allowNetwork, log) {
  const partition = viewPartition(rootReal, allowNetwork);
  const have = htmlSessions.get(partition);
  if (have) return have;
  const ses = session.fromPartition(partition); // no "persist:" => in memory
  ses.protocol.handle('file', async (req) => {
    let p;
    try {
      p = await fs.promises.realpath(fileURLToPath(req.url));
    } catch {
      return new Response('not found', { status: 404 });
    }
    if (!within(rootReal, p)) return new Response('outside the view directory', { status: 403 });
    return net.fetch(pathToFileURL(p).toString(), { bypassCustomProtocolHandlers: true });
  });
  let ready = Promise.resolve();
  if (!allowNetwork) {
    ses.webRequest.onBeforeRequest({ urls: NETWORK_URLS }, (_d, cb) => cb({ cancel: true }));
    ready = ses.setProxy(DEAD_PROXY).catch(() => {});
  }
  ses.setPermissionRequestHandler((_wc, _perm, cb) => cb(false));
  ses.setPermissionCheckHandler(() => false);
  guardDownloads(ses);
  webUserAgent(ses, log);
  const out = { ses, rootReal, ready, partition };
  htmlSessions.set(partition, out);
  return out;
}

// A permission request's grant keys: one per media type (a microphone grant is not a camera
// grant), else one for the permission.
function grantKeys(origin, permission, details = {}) {
  if (permission !== 'media') return [`${origin}|${permission}`];
  const types = (details.mediaTypes && details.mediaTypes.length ? details.mediaTypes : [details.mediaType]).filter(
    (t) => t === 'audio' || t === 'video',
  );
  return types.length ? [...new Set(types)].sort().map((t) => `${origin}|media:${t}`) : [`${origin}|media:unknown`];
}

// A frame's identity for its dialogs (WebFrameMain): its frame tree node, stable across its
// documents' processes.
function frameKey(frame) {
  try {
    return frame ? `${frame.frameTreeNodeId}` : null;
  } catch {
    return null;
  }
}

async function targetIdOf(wc) {
  let dbg = null;
  let attached = false;
  try {
    dbg = wc.debugger;
    if (!dbg.isAttached()) {
      dbg.attach('1.3');
      attached = true;
    }
    const info = await dbg.sendCommand('Target.getTargetInfo');
    return info.targetInfo.targetId;
  } catch {
    return null;
  } finally {
    if (attached) {
      try {
        dbg.detach();
      } catch {}
    }
  }
}

// A record's webContents while alive, else null (destroyed: undefined, or isDestroyed()).
function liveWc(rec) {
  const wc = rec && rec.view && rec.view.webContents;
  if (!wc) return null;
  try {
    return wc.isDestroyed() ? null : wc;
  } catch {
    return null;
  }
}

class ViewManager {
  // hooks: {log(...args), onNav(viewId, patch), onState(viewId), onError(viewId, kind, text), onOpen(fromViewId, url, disposition), onKey(input) -> bool,
  //         onPermission(viewId, {permission,label,origin}, done(allow)), onLeave(viewId, newOrigin|null),
  //         onDialogsEnd(viewId, frameKey|null): its dialogs are cancelled (navigation, crash, destroy),
  //         onGone(viewId): its WebContents went without destroy() (window.close(), a CDP close),
  //         headless: popups never show, popupLevel() -> {raised, allSpaces} | null: the level popups take (the dock's),
  //         onPopupContextMenu(ownerViewId, win, params)}
  constructor(hooks) {
    this.hooks = hooks;
    this.log = hooks.log || (() => {});
    this.live = new Map(); // view_id -> {view, entry, rootReal}
    this.host = null; // the window the views live in (the dock's while docked)
    this.overlays = []; // views kept above every page (the dialog modal); they move with the pages
    this.popups = new Map(); // BrowserWindow -> {owner: view_id}: pages' sign-in popups (src/popups.js)
  }

  // A view that stays above the pages in whatever window hosts them.
  addOverlay(view, win) {
    this.overlays.push(view);
    (this.host || win).contentView.addChildView(view);
  }

  // Adding a view again moves it to the top: the overlays above a page added after them, and
  // above them what the host keeps on top of everything (the dock's bar: hooks.onRaise).
  raiseOverlays(win) {
    const host = this.host || win;
    if (!host) return;
    for (const v of this.overlays) host.contentView.addChildView(v);
    if (this.hooks.onRaise) this.hooks.onRaise(host);
  }

  // Move every view into `win` (docked mode and back). Re-parented views keep painting
  // (spike S3); their page state is untouched.
  setHost(win) {
    if (!win || this.host === win) return;
    const old = this.host;
    this.host = win;
    for (const rec of this.live.values()) {
      try {
        if (old && !old.isDestroyed()) old.contentView.removeChildView(rec.view);
      } catch {}
      win.contentView.addChildView(rec.view);
    }
    for (const v of this.overlays) {
      try {
        if (old && !old.isDestroyed()) old.contentView.removeChildView(v);
      } catch {}
      win.contentView.addChildView(v);
    }
    if (this.hooks.onRaise) this.hooks.onRaise(win);
    this.syncPopups();
  }

  // ---------- popups (src/popups.js) ----------

  // A page's window.open, target=_blank, cmd+click (and those of its popups, on behalf of the
  // view that opened them): a sign-in popup becomes a child window of the companion's with
  // its opener (a live window handle, window.opener); anything else a new view in the same
  // group; never data: or a local file.
  _openHandler(ownerId) {
    return (details) => {
      const kind = openKind(details);
      if (kind === 'popup') {
        const parent = this.host && !this.host.isDestroyed() ? this.host : null;
        if (parent) {
          return {
            action: 'allow',
            overrideBrowserWindowOptions: popupWindowOptions({ parent, show: !this.hooks.headless, preload: PAGE_PRELOAD }),
          };
        }
      }
      if (kind !== 'deny') this.hooks.onOpen(ownerId, details.url, details.disposition);
      return { action: 'deny' };
    };
  }

  // A popup window was created for a view (or for one of its popups): tracked until it closes
  // (its own window.close() included; Electron closes it with its opener), the same open
  // policy and context menu, the companion's level.
  _adoptPopup(win, ownerId) {
    const wc = win.webContents;
    this.popups.set(win, { owner: ownerId });
    win.on('closed', () => this.popups.delete(win));
    wc.setWindowOpenHandler(this._openHandler(ownerId));
    wc.on('did-create-window', (child) => this._adoptPopup(child, ownerId));
    wc.on('context-menu', (_e, params) => this.hooks.onPopupContextMenu && this.hooks.onPopupContextMenu(ownerId, win, params));
    this._popupLevel(win);
    this.log('popup opened', ownerId);
  }

  // The popups follow the window the pages are in (the dock's while docked) and its level and
  // Spaces: above it, never behind the raised dock, gone with it.
  syncPopups() {
    for (const win of [...this.popups.keys()]) {
      if (win.isDestroyed()) {
        this.popups.delete(win);
        continue;
      }
      try {
        if (this.host && !this.host.isDestroyed() && win.getParentWindow() !== this.host) win.setParentWindow(this.host);
      } catch (e) {
        this.log('popup re-parent failed', e && e.message);
      }
      this._popupLevel(win);
    }
  }

  _popupLevel(win) {
    const lvl = this.hooks.popupLevel ? this.hooks.popupLevel() : null;
    if (!lvl) return;
    try {
      if (lvl.raised) win.setAlwaysOnTop(true, 'normal', 1);
      else win.setAlwaysOnTop(false);
      win.setVisibleOnAllWorkspaces(!!lvl.allSpaces, { visibleOnFullScreen: !!lvl.allSpaces, skipTransformProcessType: true });
    } catch (e) {
      this.log('popup level failed', e && e.message);
    }
  }

  // The popup window that `win` is (a BrowserWindow), else null.
  popupOf(win) {
    return win && this.popups.has(win) && !win.isDestroyed() ? win : null;
  }

  // Every open popup: {owner, url, bounds, parent, visible} (the test hook's view).
  popupList() {
    const out = [];
    for (const [win, p] of this.popups) {
      if (win.isDestroyed()) continue;
      let parent = null;
      try {
        parent = win.getParentWindow();
      } catch {}
      out.push({ owner: p.owner, url: win.webContents.getURL(), bounds: win.getBounds(), parent_is_host: !!parent && parent === this.host, visible: win.isVisible(), always_on_top: win.isAlwaysOnTop() });
    }
    return out;
  }

  closePopups() {
    for (const win of [...this.popups.keys()]) {
      try {
        if (!win.isDestroyed()) win.close();
      } catch {}
    }
  }

  get(viewId) {
    return this.live.get(viewId) || null;
  }

  // The view's webContents while it is alive, else null. A page's window.close() or a CDP
  // close (Target.closeTarget, Page.close) destroys it under us: from then on
  // WebContentsView.webContents is undefined.
  wcOf(viewId) {
    return liveWc(this.live.get(viewId));
  }

  // The view whose webContents is `wc`, or null.
  viewIdOf(wc) {
    if (!wc) return null;
    for (const [id, rec] of this.live) if (liveWc(rec) === wc) return id;
    return null;
  }

  // The persistent web partition with the permission policy: PROMPTED permissions go to
  // hooks.onPermission(viewId, {permission, label, origin}, cb) (main asks in the strip;
  // allow once grants this request and the page's checks until it leaves the origin).
  _webSession() {
    const ses = session.fromPartition(WEB_PARTITION);
    if (this.webSessionReady) return ses;
    this.webSessionReady = true;
    guardDownloads(ses);
    webUserAgent(ses, this.log);
    ses.setPermissionRequestHandler((wc, permission, cb, details = {}) => {
      if (ALLOWED.has(permission)) return cb(true);
      const viewId = this.viewIdOf(wc);
      if (!PROMPTED.has(permission) || !viewId || !this.hooks.onPermission) return cb(false);
      const origin = originOf(details.requestingUrl || (wc && wc.getURL()) || '');
      if (!origin || origin === 'null') return cb(false);
      const rec = this.live.get(viewId);
      const keys = grantKeys(origin, permission, details);
      if (keys.every((k) => rec.grants.has(k))) return cb(true);
      let label = permission;
      if (permission === 'media') {
        const t = details.mediaTypes || [];
        label = t.includes('video') && t.includes('audio') ? 'use your camera and microphone' : t.includes('video') ? 'use your camera' : 'use your microphone';
      } else if (permission === 'clipboard-read') label = 'read the clipboard';
      else if (permission === 'notifications') label = 'show notifications';
      else if (permission === 'geolocation') label = 'know your location';
      this.hooks.onPermission(viewId, { permission, label, origin, keys }, (allow) => {
        if (allow && this.live.get(viewId) === rec) for (const k of keys) rec.grants.add(k);
        cb(!!allow);
      });
    });
    ses.setPermissionCheckHandler((wc, permission, requestingOrigin, details = {}) => {
      if (ALLOWED.has(permission)) return true;
      if (!PROMPTED.has(permission)) return false;
      const viewId = this.viewIdOf(wc);
      const rec = viewId && this.live.get(viewId);
      return !!(rec && grantKeys(originOf(requestingOrigin), permission, details).every((k) => rec.grants.has(k)));
    });
    return ses;
  }

  has(viewId) {
    return this.live.has(viewId);
  }

  // Creates the WebContentsView for an entry (idempotent). Resolves with the CDP target id.
  async create(entry, win) {
    const have = this.live.get(entry.view_id);
    if (have) return have.target;
    const isHtml = entry.kind === 'html_view';
    let ses;
    let rootReal = null;
    let sesReady = null;
    const noNetwork = isHtml && !entry.allow_network;
    // page API (src/page-api.js): window.herdr in the top frame of an opted-in view, while
    // herdr advertises page messages
    const pageApi = noNetwork && !!entry.page_api && /^agent:./.test(entry.opened_by || '') && !!(this.hooks.pageApiOn && this.hooks.pageApiOn());
    const extraArgs = [...(noNetwork ? ['--herdr-nonet'] : []), ...(pageApi ? ['--herdr-page-api'] : [])];
    if (isHtml) {
      ({ ses, rootReal, ready: sesReady } = htmlSession(viewRoot(entry.root, entry.file), entry.allow_network, this.log));
    } else {
      ses = this._webSession();
    }
    const view = new WebContentsView({
      webPreferences: {
        session: ses,
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
        backgroundThrottling: false,
        webSecurity: true,
        // sandboxed (no Node in the page); in subframes too, so a frame's dialogs are the companion's
        preload: PAGE_PRELOAD,
        nodeIntegrationInSubFrames: true,
        ...(extraArgs.length ? { additionalArguments: extraArgs } : {}),
      },
    });
    const wc = view.webContents; // kept: the view's own property goes when the page closes itself
    if (noNetwork) {
      try {
        wc.setWebRTCIPHandlingPolicy('disable_non_proxied_udp');
      } catch {}
    }
    // pageApi: the bridge was given; pageToken: the current document's (a new document asks
    // for a new one: did-navigate clears it)
    const rec = { view, entry, rootReal, target: null, ready: null, grants: new Set(), pageApi, pageToken: null };
    this.live.set(entry.view_id, rec);
    view.setVisible(false);
    (this.host || win).contentView.addChildView(view);
    this.raiseOverlays(win);
    this._wire(rec);
    rec.ready = (async () => {
      // Never leave a target unloaded: an unloaded webContents stalls connectOverCDP (spike S1).
      if (sesReady) await sesReady;
      if (wc.isDestroyed()) return null;
      await wc.loadURL(`about:blank#herdr-view=${entry.view_id}`).catch(() => {});
      if (wc.isDestroyed()) return null;
      rec.target = await targetIdOf(wc);
      const url = isHtml ? pathToFileURL(entry.file).toString() : entry.url;
      if (url && url !== 'about:blank' && !wc.isDestroyed()) {
        wc.loadURL(url).catch(() => {}); // failures surface as did-fail-load
      }
      return rec.target;
    })();
    return rec.ready;
  }

  _wire(rec) {
    const { view, entry } = rec;
    const wc = view.webContents;
    const id = entry.view_id;
    // destroyed without destroy(): the page called window.close() (Electron closes a page's
    // WebContents for it, whoever opened it) or a CDP client closed its target. The page went:
    // the same bookkeeping as destroy().
    wc.once('destroyed', () => this._gone(id, rec));
    const nav = () => {
      const url = wc.getURL();
      if (url.startsWith('about:blank#herdr-view=')) return;
      const origin = originOf(url);
      for (const g of [...rec.grants]) if (!g.startsWith(origin + '|')) rec.grants.delete(g);
      if (this.hooks.onLeave) this.hooks.onLeave(id, origin);
      this.hooks.onNav(id, { url });
    };
    wc.on('did-navigate', () => {
      rec.pageToken = null; // a committed main-frame navigation: a new document
      nav();
    });
    wc.on('did-navigate-in-page', (_e, _url, isMainFrame) => isMainFrame && nav());
    wc.on('page-title-updated', (_e, title) => this.hooks.onNav(id, { title }));
    wc.on('did-start-loading', () => this.hooks.onState(id));
    wc.on('did-stop-loading', () => this.hooks.onState(id));
    wc.on('did-fail-load', (_e, code, desc, url, isMainFrame) => {
      if (isMainFrame && code !== -3) this.hooks.onError(id, 'load_failed', `${desc} (${code}) ${url}`);
    });
    wc.on('render-process-gone', (_e, d) => {
      if (this.hooks.onDialogsEnd) this.hooks.onDialogsEnd(id, null);
      this.hooks.onError(id, 'crashed', d.reason);
    });
    // a frame leaving its document cancels its dialogs and those of the frames inside it (the
    // main frame: every one of the view)
    wc.on('did-start-navigation', (d) => {
      if (!d || d.isSameDocument || !this.hooks.onDialogsEnd) return;
      if (d.isMainFrame) return this.hooks.onDialogsEnd(id, null);
      const key = frameKey(d.frame);
      if (key) this.hooks.onDialogsEnd(id, key); // no frame: nothing to match (never all of them)
    });
    wc.on('before-input-event', (e, input) => {
      if (this.hooks.onKey(input)) e.preventDefault();
    });
    // a right-click: the page's context menu (main.js)
    wc.on('context-menu', (_e, params) => this.hooks.onContextMenu && this.hooks.onContextMenu(id, params));
    if (entry.kind === 'html_view') {
      wc.setWindowOpenHandler(() => ({ action: 'deny' }));
      wc.on('will-navigate', (e, url) => {
        if (url.startsWith('file:')) {
          let p;
          try {
            p = fileURLToPath(url);
          } catch {
            p = '';
          }
          if (rec.rootReal && p && within(rec.rootReal, path.resolve(p))) return;
          e.preventDefault();
          return;
        }
        e.preventDefault();
        // a web tab the view's page asked for: on behalf of whoever opened the view
        if (/^https?:/i.test(url)) this.hooks.onOpen(id, url, 'background-tab', { opened_by: entry.opened_by || 'agent', fromView: true });
      });
    } else {
      // window.open / target=_blank / cmd+click: a sign-in popup is a child window with its
      // opener, anything else a new view in the same group (src/popups.js)
      wc.setWindowOpenHandler(this._openHandler(id));
      wc.on('did-create-window', (win) => this._adoptPopup(win, id));
    }
  }

  destroy(viewId, win) {
    const rec = this.live.get(viewId);
    if (!rec) return;
    this._forget(viewId, rec, win);
    try {
      const wc = liveWc(rec);
      if (wc) wc.close();
    } catch {}
  }

  // Out of `live` with its prompts and dialogs ended and its view out of the window (the
  // record is gone first: nothing that runs from here on finds it).
  _forget(viewId, rec, win) {
    this.live.delete(viewId);
    try {
      if (this.hooks.onLeave) this.hooks.onLeave(viewId, null);
    } catch (e) {
      this.log('view leave failed', viewId, e && e.message);
    }
    // its blocked dialog calls are answered before the page goes
    try {
      if (this.hooks.onDialogsEnd) this.hooks.onDialogsEnd(viewId, null);
    } catch (e) {
      this.log('view dialogs end failed', viewId, e && e.message);
    }
    try {
      (this.host || win).contentView.removeChildView(rec.view);
    } catch {}
  }

  // Its WebContents was destroyed without destroy() (window.close(), a CDP close): forgotten as destroy()
  // does, then main hears it (hooks.onGone) and closes its tab.
  _gone(viewId, rec) {
    if (this.live.get(viewId) !== rec) return; // destroy() went first
    this.log('view gone (its page was closed outside the companion)', viewId);
    this._forget(viewId, rec, null);
    if (this.hooks.onGone) this.hooks.onGone(viewId);
  }

  navState(viewId) {
    const wc = this.wcOf(viewId);
    if (!wc) return { canGoBack: false, canGoForward: false, loading: false };
    try {
      const h = wc.navigationHistory;
      return { canGoBack: h.canGoBack(), canGoForward: h.canGoForward(), loading: wc.isLoading() };
    } catch {
      return { canGoBack: false, canGoForward: false, loading: false };
    }
  }

  // Shows exactly one view (or none) in `bounds`; every other view stays a hidden child.
  layout(shownViewId, bounds) {
    // Hidden views keep the same size so agents' screenshots match what the user would see.
    for (const [id, rec] of this.live) {
      rec.view.setBounds(bounds);
      rec.view.setVisible(id === shownViewId);
    }
  }
}

module.exports = { ViewManager, frameKey, WEB_PARTITION, within, PROMPTED, ALLOWED, grantKeys, viewRoot, viewPartition };
