// One WebContentsView per page (contract §6). Web pages share the persistent profile
// partition; every html view gets its own in-memory sandbox partition (§4.3).
'use strict';
const { WebContentsView, session, net } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL, fileURLToPath } = require('node:url');

const WEB_PARTITION = 'persist:herdr-companion';
const NETWORK_URLS = ['http://*/*', 'https://*/*', 'ws://*/*', 'wss://*/*'];

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

// html_view sandbox: file:// limited to `root` (canonicalized, no symlink escape),
// network blocked unless allowed, every permission denied.
function htmlSession(viewId, root, allowNetwork) {
  const ses = session.fromPartition(`herdr-view-${viewId}`); // no "persist:" => in memory
  let rootReal = root;
  try {
    rootReal = fs.realpathSync(root);
  } catch {}
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
  if (!allowNetwork) {
    ses.webRequest.onBeforeRequest({ urls: NETWORK_URLS }, (_d, cb) => cb({ cancel: true }));
  }
  ses.setPermissionRequestHandler((_wc, _perm, cb) => cb(false));
  ses.setPermissionCheckHandler(() => false);
  return { ses, rootReal };
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
    ses.setPermissionRequestHandler((wc, permission, cb, details = {}) => {
      if (ALLOWED.has(permission)) return cb(true);
      const viewId = this.viewIdOf(wc);
      if (!PROMPTED.has(permission) || !viewId || !this.hooks.onPermission) return cb(false);
      const origin = originOf(details.requestingUrl || (wc && wc.getURL()) || '');
      if (!origin || origin === 'null') return cb(false);
      const rec = this.live.get(viewId);
      if (rec.grants.has(`${origin}|${permission}`)) return cb(true);
      let label = permission;
      if (permission === 'media') {
        const t = details.mediaTypes || [];
        label = t.includes('video') && t.includes('audio') ? 'use your camera and microphone' : t.includes('video') ? 'use your camera' : 'use your microphone';
      } else if (permission === 'clipboard-read') label = 'read the clipboard';
      else if (permission === 'notifications') label = 'show notifications';
      else if (permission === 'geolocation') label = 'know your location';
      this.hooks.onPermission(viewId, { permission, label, origin }, (allow) => {
        if (allow && this.live.get(viewId) === rec) rec.grants.add(`${origin}|${permission}`);
        cb(!!allow);
      });
    });
    ses.setPermissionCheckHandler((wc, permission, requestingOrigin) => {
      if (ALLOWED.has(permission)) return true;
      if (!PROMPTED.has(permission)) return false;
      const viewId = this.viewIdOf(wc);
      const rec = viewId && this.live.get(viewId);
      return !!(rec && rec.grants.has(`${originOf(requestingOrigin)}|${permission}`));
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
    if (isHtml) {
      const root = entry.root || path.dirname(entry.file);
      ({ ses, rootReal } = htmlSession(entry.view_id, root, entry.allow_network));
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
      },
    });
    const rec = { view, entry, rootReal, target: null, ready: null, grants: new Set() };
    this.live.set(entry.view_id, rec);
    view.setVisible(false);
    win.contentView.addChildView(view);
    this._wire(rec);
    rec.ready = (async () => {
      // Never leave a target unloaded: an unloaded webContents stalls connectOverCDP (spike S1).
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
        if (/^https?:/i.test(url)) this.hooks.onOpen(id, url, 'background-tab');
      });
    } else {
      // window.open / target=_blank / cmd+click: a new view in the same group, never a window.
      wc.setWindowOpenHandler(({ url, disposition }) => {
        if (/^(https?|about|data):/i.test(url)) this.hooks.onOpen(id, url, disposition);
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
      win.contentView.removeChildView(rec.view);
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

module.exports = { ViewManager, WEB_PARTITION, within, PROMPTED, ALLOWED };
