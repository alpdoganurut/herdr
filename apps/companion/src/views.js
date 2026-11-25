// One WebContentsView per page (contract §6). Web pages share the persistent profile
// partition; every html view gets its own in-memory sandbox partition (§4.3).
'use strict';
const { WebContentsView, session, net, app, screen } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { pathToFileURL, fileURLToPath } = require('node:url');

const WEB_PARTITION = 'persist:herdr-companion';
const NETWORK_URLS = ['http://*/*', 'https://*/*', 'ws://*/*', 'wss://*/*'];
// every page view, every frame: the companion's alert/confirm/prompt (src/dialogs.js) and, in
// html views without network, no WebRTC
const PAGE_PRELOAD = path.join(__dirname, 'preload', 'page.js');
const remote = require('./remote.js');
const { FILE_SCHEME, cleanText, isFileToken, parseFileUrl, fileResponse } = require('./remote-rules.js');
const { downloadAllowed } = require('./context-menu.js');
const { openKind, popupWindowOptions, clampBounds, POPUP_CAP } = require('./popups.js');
// The longest a hidden view paints for one capture hold (holdPaint); the sidecar asks for
// what is left of its screenshot's deadline and lets go when its captures settle.
const PAINT_HOLD_MAX_MS = 60000;
// A remote companion's page that waits this long for its proxy reports load_failed.
const GATE_REPORT_MS = 600;
// Its first load that failed on the proxy is tried again this long after the proxy answers.
const GATE_RETRY_MS = 1000;

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
// network blocked unless allowed, every permission denied. Set up once per partition. A remote
// companion's serves one shown file's token (`far`: {token, read(rel, offset)}), no file here.
const htmlSessions = new Map(); // partition -> {ses, rootReal, ready}
function htmlSession(rootReal, allowNetwork, log, far = null) {
  const partition = viewPartition(rootReal, allowNetwork);
  const have = htmlSessions.get(partition);
  if (have) return have;
  const ses = session.fromPartition(partition); // no "persist:" => in memory
  // a remote companion's sessions refuse file: already (remote.js), and it stays refused
  if (!remote.on) {
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
  } else if (far) {
    const down = `${remote.machine}: no connection; this page loads when it is back`;
    ses.protocol.handle(FILE_SCHEME, (req) => fileResponse(req, { token: far.token, pull: far.read, down }));
  }
  let ready = Promise.resolve();
  if (!allowNetwork) {
    ses.webRequest.onBeforeRequest({ urls: NETWORK_URLS }, (_d, cb) => cb({ cancel: true }));
    ready = ses.setProxy(DEAD_PROXY).catch(() => {});
    remote.offline(ses);
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
  //         readFile(pageToken, rel, offset): a remote companion's shown file (a link.file answer),
  //         onGone(viewId): its WebContents went without destroy() (window.close(), a CDP close),
  //         headless: popups never show, popupLevel() -> {raised, allSpaces} | null: the level popups take (the dock's),
  //         onPopupContextMenu(ownerViewId, win, params)}
  constructor(hooks) {
    this.hooks = hooks;
    this.log = hooks.log || (() => {});
    this.live = new Map(); // view_id -> {view, entry, rootReal}
    this.host = null; // the window the views live in (the dock's while docked)
    this.overlays = []; // views kept above every page (the dialog modal); they move with the pages
    this.popups = new Map(); // BrowserWindow -> {owner: view_id, ...}: pages' sign-in popups (src/popups.js)
    this.popupPending = new Map(); // owner view_id -> times of allowed opens not yet created (the cap counts them)
    this.popupShown = null; // the view ids whose popups may show (the shown herdr tab's group); null: all
    this.autoAnswered = new Set(); // `${kind}|${origin}` already logged (a popup's dialog or permission)
    // the last layout(): {shownViewId, bounds}; null until the first (views are placed by it)
    this.placed = null;
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
    // a view held for a capture goes back under the pages
    for (const [id, rec] of this.live) if (rec.paintHeld) this._place(id, rec);
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
        if (parent && this._popupCount(ownerId) >= POPUP_CAP) {
          this.log('popup refused (cap', POPUP_CAP, 'reached), opened as a tab:', originOf(details.url) || '-');
        } else if (parent) {
          this._pending(ownerId).push(Date.now());
          const show = !this.hooks.headless && this._popupMayShow(ownerId);
          return {
            action: 'allow',
            overrideBrowserWindowOptions: popupWindowOptions({ parent, show, preload: PAGE_PRELOAD, devTools: !remote.on }),
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
  _adoptPopup(win, ownerId, details = {}) {
    const wc = win.webContents;
    const pending = this._pending(ownerId);
    pending.shift();
    const opts = details.options || {};
    this.popups.set(win, { owner: ownerId, frame: opts.frame !== false, transparent: !!opts.transparent, hidden: false });
    win.on('closed', () => this.popups.delete(win));
    wc.setWindowOpenHandler(this._openHandler(ownerId));
    wc.on('did-create-window', (child, d) => this._adoptPopup(child, ownerId, d));
    wc.on('context-menu', (_e, params) => this.hooks.onPopupContextMenu && this.hooks.onPopupContextMenu(ownerId, win, params));
    // a page's beforeunload never keeps a popup open: ⌘W, its opener's tab closing and the
    // companion's quit always close it
    wc.on('will-prevent-unload', (e) => e.preventDefault());
    // the title says where the page is from (a popup can look like anything): `<host> — <title>`
    const title = (t) => {
      let host = '';
      try {
        host = new URL(wc.getURL()).host;
      } catch {}
      try {
        if (win.isDestroyed()) return;
        // the machine's name first: the rest is the page's
        if (remote.on) win.setTitle([remote.machine, host, cleanText(t)].filter(Boolean).join(' — '));
        else win.setTitle(host ? `${host} — ${t || ''}` : t || '');
      } catch {}
    };
    win.on('page-title-updated', (e, t) => {
      e.preventDefault();
      title(t);
    });
    wc.on('did-navigate', () => title(wc.getTitle()));
    title(wc.getTitle());
    // inside its display's work area, never larger than POPUP_MAX
    try {
      const b = win.getBounds();
      const area = screen.getDisplayMatching(this.host && !this.host.isDestroyed() ? this.host.getBounds() : b).workArea;
      const c = clampBounds(b, area);
      if (c.x !== b.x || c.y !== b.y || c.width !== b.width || c.height !== b.height) win.setBounds(c);
    } catch (e) {
      this.log('popup clamp failed', e && e.message);
    }
    this._popupLevel(win);
    this._popupVisibility(win);
    this.log('popup opened', ownerId);
  }

  // Allowed opens of `ownerId` not created yet (the window comes at did-create-window; one
  // that never comes stops counting after a while).
  _pending(ownerId) {
    const now = Date.now();
    const list = (this.popupPending.get(ownerId) || []).filter((t) => now - t < 10000);
    this.popupPending.set(ownerId, list);
    return list;
  }

  _popupCount(ownerId) {
    let n = this._pending(ownerId).length;
    for (const [win, p] of this.popups) if (p.owner === ownerId && !win.isDestroyed()) n++;
    return n;
  }

  _popupMayShow(ownerId) {
    return !this.popupShown || this.popupShown.has(ownerId);
  }

  // The popups of the shown herdr tab's pages show; the others hide until their tab shows
  // again (`viewIds`: the shown group's view ids; null: none shows).
  showPopupsOf(viewIds) {
    this.popupShown = new Set(viewIds || []);
    for (const win of this.popups.keys()) this._popupVisibility(win);
  }

  _popupVisibility(win) {
    const p = this.popups.get(win);
    if (!p || win.isDestroyed()) return;
    const hide = !this._popupMayShow(p.owner);
    if (hide === p.hidden) return;
    p.hidden = hide;
    if (this.hooks.headless) return;
    try {
      if (hide) win.hide();
      else win.showInactive();
    } catch (e) {
      this.log('popup show/hide failed', e && e.message);
    }
  }

  // A popup's dialog or prompted permission is answered without asking (no strip or card for
  // popups): logged once per kind and origin.
  noteAutoAnswer(kind, wc) {
    let origin = '';
    try {
      origin = originOf(wc.getURL());
    } catch {}
    const key = `${kind}|${origin}`;
    if (this.autoAnswered.has(key)) return;
    this.autoAnswered.add(key);
    this.log(`popup ${kind} answered without asking (popups get none):`, origin || '-');
  }

  // Whether `wc` is a popup's.
  isPopupWc(wc) {
    for (const win of this.popups.keys()) {
      try {
        if (!win.isDestroyed() && win.webContents === wc) return true;
      } catch {}
    }
    return false;
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
      if (lvl.raised) win.setAlwaysOnTop(true, ...lvl.level);
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
      out.push({
        owner: p.owner,
        url: win.webContents.getURL(),
        title: win.getTitle(),
        bounds: win.getBounds(),
        parent_is_host: !!parent && parent === this.host,
        visible: win.isVisible(),
        hidden_with_tab: p.hidden,
        always_on_top: win.isAlwaysOnTop(),
        frame: p.frame,
        transparent: p.transparent,
        closable: win.isClosable(),
        movable: win.isMovable(),
        resizable: win.isResizable(),
        opacity: win.getOpacity(),
      });
    }
    return out;
  }

  closePopups() {
    for (const win of [...this.popups.keys()]) {
      try {
        if (!win.isDestroyed()) win.destroy(); // never asks the page (beforeunload)
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

  // A hidden view paints for an agent's capture for `ms` (at most PAINT_HOLD_MAX_MS; a new
  // call extends the hold, `ms` 0 lets it go). Chromium draws no frame for a hidden view: a
  // CDP capture of one waits for a frame that never comes (shown before, a few seconds after
  // it was hidden; never shown, its renderer has no size at all). Held, it is visible at the
  // shown page's bounds, under every other page: the shown page covers it, so the user sees
  // nothing change. Not while no page is shown (nothing would cover it). Answers whether the
  // view paints now; null: no such view.
  holdPaint(viewId, ms) {
    const rec = this.live.get(viewId);
    if (!liveWc(rec)) return null;
    const hold = Math.min(Math.max(0, Number(ms) || 0), PAINT_HOLD_MAX_MS);
    if (rec.paintTimer) clearTimeout(rec.paintTimer);
    rec.paintTimer = null;
    if (!hold) {
      this._releasePaint(viewId, rec);
      return false;
    }
    // nothing shown to cover it: not held (the capture goes as it would have)
    const p = this.placed;
    if (!p || !p.shownViewId || !this.live.has(p.shownViewId)) {
      this._releasePaint(viewId, rec);
      return false;
    }
    rec.paintTimer = setTimeout(() => this._releasePaint(viewId, rec), hold);
    if (!rec.paintHeld) {
      rec.paintHeld = true;
      this._place(viewId, rec);
    }
    return true;
  }

  _releasePaint(viewId, rec) {
    if (rec.paintTimer) clearTimeout(rec.paintTimer);
    rec.paintTimer = null;
    if (!rec.paintHeld) return;
    rec.paintHeld = false;
    if (this.live.get(viewId) === rec) this._place(viewId, rec);
  }

  // The pages in the host window's stacking order, bottom first: {view_id, visible} (a test
  // hook: a view held for a capture must sit under the shown one).
  stack() {
    const children = this.host ? this.host.contentView.children : [];
    const out = [];
    for (const child of children) {
      for (const [id, rec] of this.live) if (rec.view === child) out.push({ view_id: id, visible: rec.view.getVisible() });
    }
    return out;
  }

  // The view whose webContents is `wc`, or null.
  viewIdOf(wc) {
    if (!wc) return null;
    for (const [id, rec] of this.live) if (liveWc(rec) === wc) return id;
    return null;
  }

  // The persistent web partition with the permission policy: PROMPTED permissions go to
  // hooks.onPermission(viewId, {permission, label, origin}, cb) (main asks in the strip;
  // allow once grants this request and the page's checks until it leaves the origin). A remote
  // companion denies them all.
  webSession() {
    const ses = session.fromPartition(WEB_PARTITION);
    if (this.webSessionReady) return ses;
    this.webSessionReady = true;
    guardDownloads(ses);
    webUserAgent(ses, this.log);
    if (remote.on) return ses; // denied for every session (remote.js)
    ses.setPermissionRequestHandler((wc, permission, cb, details = {}) => {
      if (ALLOWED.has(permission)) return cb(true);
      const viewId = this.viewIdOf(wc);
      if (PROMPTED.has(permission) && !viewId && this.isPopupWc(wc)) this.noteAutoAnswer(`permission ${permission}`, wc);
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
    const noNetwork = isHtml && (!entry.allow_network || remote.on); // a remote html view never has network
    // page API (src/page-api.js): window.herdr in the top frame of an opted-in view, while
    // herdr advertises page messages
    const pageApi = noNetwork && !remote.on && !!entry.page_api && /^agent:./.test(entry.opened_by || '') && !!(this.hooks.pageApiOn && this.hooks.pageApiOn());
    const extraArgs = [...(noNetwork ? ['--herdr-nonet'] : []), ...(pageApi ? ['--herdr-page-api'] : [])];
    if (isHtml && remote.on) {
      // another machine's file, under the token its page is loaded with: no file here is looked
      // at, every file: URL is refused
      const token = isFileToken(entry.serve) ? entry.serve : null;
      const far = token && { token, read: (rel, offset) => this.hooks.readFile(token, rel, offset) };
      entry.page_token = token;
      ({ ses, ready: sesReady } = htmlSession(`remote ${token || entry.view_id}`, false, this.log, far));
    } else if (isHtml) {
      ({ ses, rootReal, ready: sesReady } = htmlSession(viewRoot(entry.root, entry.file), entry.allow_network, this.log));
    } else {
      ses = this.webSession();
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
        ...remote.prefs,
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
    // paintHeld/paintTimer: a hold for an agent's capture (holdPaint)
    // waiting: the URL a remote companion's page loads once its proxy answers; committed: it
    // showed a page already; down: 'waits' (loads by itself once the other machine answers) or
    // 'failed' (its page did not load), for the bar
    const rec = { view, entry, rootReal, target: null, ready: null, grants: new Set(), pageApi, pageToken: null, paintHeld: false, paintTimer: null, waiting: null, committed: false, down: null };
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
      const url = isHtml && !remote.on ? pathToFileURL(entry.file).toString() : entry.url;
      if (url && url !== 'about:blank' && !wc.isDestroyed()) {
        // never waited for: a proxy that is down must not hold `ready`
        this._load(rec, wc, url);
      }
      return rec.target;
    })();
    return rec.ready;
  }

  // A page's load. A remote companion's web page waits for its session's gate (remote.js); one
  // whose proxy is down stays blank (or on its error) with its URL set and loads by itself when
  // the proxy is back, unless it was navigated or closed meanwhile.
  // `retry`: its load failed already (and was reported).
  _load(rec, wc, url, { retry = false } = {}) {
    const gate = rec.entry.kind === 'html_view' ? null : remote.gate(wc.session);
    if (!gate) {
      wc.loadURL(url).catch(() => {}); // failures surface as did-fail-load
      return;
    }
    rec.waiting = url;
    const waits = () => rec.waiting === url && this.live.get(rec.entry.view_id) === rec;
    const slow = setTimeout(() => {
      if (waits()) this._down(rec, 'waits');
      if (!retry && waits()) this.hooks.onError(rec.entry.view_id, 'load_failed', `the browser's proxy does not answer (${remote.why(wc.session)}); it loads when the proxy is back: ${url}`);
    }, GATE_REPORT_MS);
    const go = () => {
      if (!waits() || wc.isDestroyed()) return;
      rec.waiting = null;
      wc.loadURL(url).catch(() => {});
    };
    gate.then(() => {
      clearTimeout(slow);
      if (retry) setTimeout(go, GATE_RETRY_MS);
      else go();
    });
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
    wc.on('did-navigate', (_e, _url, status) => {
      rec.pageToken = null; // a committed main-frame navigation: a new document
      // a shown file whose other machine did not answer: linkUp() loads it again
      if (!rec.waiting) this._down(rec, remote.on && entry.kind === 'html_view' && status === 502 ? 'waits' : null);
      if (!wc.getURL().startsWith('about:blank#herdr-view=')) rec.committed = true;
      nav();
    });
    wc.on('did-navigate-in-page', (_e, _url, isMainFrame) => isMainFrame && nav());
    wc.on('page-title-updated', (_e, title) => this.hooks.onNav(id, { title }));
    wc.on('did-start-loading', () => this.hooks.onState(id));
    wc.on('did-stop-loading', () => this.hooks.onState(id));
    wc.on('did-fail-load', (_e, code, desc, url, isMainFrame) => {
      if (isMainFrame && code !== -3) this.hooks.onError(id, 'load_failed', `${desc} (${code}) ${url}`);
      if (!remote.on || !isMainFrame || code === -3 || entry.kind === 'html_view') return;
      this._down(rec, 'failed');
      if (code !== remote.PROXY_DOWN) return;
      remote.lost(wc.session);
      if (!rec.committed) this._load(rec, wc, url, { retry: true }); // nothing shown yet: its first load again
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
    // navigated by someone else: what waited for the proxy is moot
    wc.on('did-start-navigation', (d) => {
      if (d && d.isMainFrame && !d.isSameDocument) {
        rec.waiting = null;
        this._down(rec, null);
      }
    });
    wc.on('before-input-event', (e, input) => {
      if (this.hooks.onKey(input)) e.preventDefault();
    });
    // a right-click: the page's context menu (main.js)
    wc.on('context-menu', (_e, params) => this.hooks.onContextMenu && this.hooks.onContextMenu(id, params));
    if (entry.kind === 'html_view') {
      wc.setWindowOpenHandler(() => ({ action: 'deny' }));
      wc.on('will-navigate', (e, url) => {
        if (remote.on && entry.page_token && (parseFileUrl(url) || {}).token === entry.page_token) return;
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
      wc.on('did-create-window', (win, d) => this._adoptPopup(win, id, d));
    }
  }

  _down(rec, state) {
    if (rec.down === state) return;
    rec.down = state;
    this.hooks.onState(rec.entry.view_id);
  }

  // Why the page is not up, for the bar: 'waits', 'failed' or null.
  downState(viewId) {
    const rec = this.live.get(viewId);
    return rec ? rec.down : null;
  }

  // The other machine answers again: the shown files that had no answer load now.
  linkUp() {
    for (const rec of this.live.values()) {
      const wc = rec.down === 'waits' && rec.entry.kind === 'html_view' ? liveWc(rec) : null;
      if (wc) wc.reload();
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
    this._releasePaint(viewId, rec);
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
  // does, then main hears it (hooks.onGone), logs it and closes its tab (not while the app quits).
  _gone(viewId, rec) {
    if (this.live.get(viewId) !== rec) return; // destroy() went first
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
    this.placed = { shownViewId, bounds };
    for (const [id, rec] of this.live) this._place(id, rec);
  }

  // One view where the last layout() puts it: hidden views keep the same size so agents'
  // screenshots match what the user would see; one held for a capture (holdPaint) is visible
  // under the shown page, and only while a live page is shown to cover it.
  _place(id, rec) {
    const p = this.placed;
    if (!p || !p.bounds) return;
    const shown = id === p.shownViewId;
    const covered = !!p.shownViewId && this.live.has(p.shownViewId);
    const held = !shown && !!rec.paintHeld && covered;
    try {
      rec.view.setBounds(p.bounds);
      // under every other page (setHost and new views add theirs on top)
      if (held && this.host) this.host.contentView.addChildView(rec.view, 0);
      rec.view.setVisible(shown || held);
    } catch (e) {
      this.log('view place failed', id, e && e.message);
    }
  }
}

module.exports = { ViewManager, frameKey, WEB_PARTITION, within, PROMPTED, ALLOWED, grantKeys, viewRoot, viewPartition };
