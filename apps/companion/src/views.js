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
const NONET_PRELOAD = path.join(__dirname, 'preload', 'nonet.js');
const { downloadAllowed } = require('./context-menu.js');

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
function htmlSession(rootReal, allowNetwork) {
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

async function targetIdOf(wc) {
  const dbg = wc.debugger;
  let attached = false;
  try {
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

class ViewManager {
  // hooks: {onNav(viewId, patch), onState(viewId), onError(viewId, kind, text), onOpen(fromViewId, url, disposition), onKey(input) -> bool,
  //         onPermission(viewId, {permission,label,origin}, done(allow)), onLeave(viewId, newOrigin|null)}
  constructor(hooks) {
    this.hooks = hooks;
    this.live = new Map(); // view_id -> {view, entry, rootReal}
    this.host = null; // the window the views live in (the dock's while docked)
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
  }

  get(viewId) {
    return this.live.get(viewId) || null;
  }

  // The view whose webContents is `wc`, or null.
  viewIdOf(wc) {
    if (!wc) return null;
    for (const [id, rec] of this.live) if (rec.view.webContents === wc) return id;
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
    if (isHtml) {
      ({ ses, rootReal, ready: sesReady } = htmlSession(viewRoot(entry.root, entry.file), entry.allow_network));
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
        // no network: WebRTC removed in every frame (the preload runs in subframes too)
        ...(noNetwork ? { preload: NONET_PRELOAD, nodeIntegrationInSubFrames: true } : {}),
      },
    });
    if (noNetwork) {
      try {
        view.webContents.setWebRTCIPHandlingPolicy('disable_non_proxied_udp');
      } catch {}
    }
    const rec = { view, entry, rootReal, target: null, ready: null, grants: new Set() };
    this.live.set(entry.view_id, rec);
    view.setVisible(false);
    (this.host || win).contentView.addChildView(view);
    this._wire(rec);
    rec.ready = (async () => {
      // Never leave a target unloaded: an unloaded webContents stalls connectOverCDP (spike S1).
      if (sesReady) await sesReady;
      await view.webContents.loadURL(`about:blank#herdr-view=${entry.view_id}`).catch(() => {});
      rec.target = await targetIdOf(view.webContents);
      const url = isHtml ? pathToFileURL(entry.file).toString() : entry.url;
      if (url && url !== 'about:blank') {
        view.webContents.loadURL(url).catch(() => {}); // failures surface as did-fail-load
      }
      return rec.target;
    })();
    return rec.ready;
  }

  _wire(rec) {
    const { view, entry } = rec;
    const wc = view.webContents;
    const id = entry.view_id;
    const nav = () => {
      const url = wc.getURL();
      if (url.startsWith('about:blank#herdr-view=')) return;
      const origin = originOf(url);
      for (const g of [...rec.grants]) if (!g.startsWith(origin + '|')) rec.grants.delete(g);
      if (this.hooks.onLeave) this.hooks.onLeave(id, origin);
      this.hooks.onNav(id, { url });
    };
    wc.on('did-navigate', nav);
    wc.on('did-navigate-in-page', (_e, _url, isMainFrame) => isMainFrame && nav());
    wc.on('page-title-updated', (_e, title) => this.hooks.onNav(id, { title }));
    wc.on('did-start-loading', () => this.hooks.onState(id));
    wc.on('did-stop-loading', () => this.hooks.onState(id));
    wc.on('did-fail-load', (_e, code, desc, url, isMainFrame) => {
      if (isMainFrame && code !== -3) this.hooks.onError(id, 'load_failed', `${desc} (${code}) ${url}`);
    });
    wc.on('render-process-gone', (_e, d) => this.hooks.onError(id, 'crashed', d.reason));
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
      // window.open / target=_blank / cmd+click: a new view in the same group, never a window.
      wc.setWindowOpenHandler(({ url, disposition }) => {
        // never data: (loaded from here it would skip Chromium's top-level data: block)
        if (/^(https?|about):/i.test(url)) this.hooks.onOpen(id, url, disposition);
        return { action: 'deny' };
      });
    }
  }

  destroy(viewId, win) {
    const rec = this.live.get(viewId);
    if (!rec) return;
    this.live.delete(viewId);
    if (this.hooks.onLeave) this.hooks.onLeave(viewId, null);
    try {
      (this.host || win).contentView.removeChildView(rec.view);
    } catch {}
    try {
      rec.view.webContents.close();
    } catch {}
  }

  navState(viewId) {
    const rec = this.live.get(viewId);
    if (!rec) return { canGoBack: false, canGoForward: false, loading: false };
    const wc = rec.view.webContents;
    const h = wc.navigationHistory;
    return { canGoBack: h.canGoBack(), canGoForward: h.canGoForward(), loading: wc.isLoading() };
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

module.exports = { ViewManager, WEB_PARTITION, within, PROMPTED, ALLOWED, grantKeys, viewRoot, viewPartition };
