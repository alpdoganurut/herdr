// herdr+ companion browser: Electron main process (contract §6).
// One window; a WebContentsView per page; one group of pages per herdr tab; follows herdr focus.
'use strict';
const { app, BrowserWindow, WebContentsView, ipcMain, protocol, session, Menu, net, nativeTheme, nativeImage } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { pathToFileURL } = require('node:url');
const { GroupStore } = require('./groups.js');
const { ViewManager, within } = require('./views.js');
const { HerdrLink } = require('./host.js');
const { resolveSocketPath, serialized } = require('./herdr-api.js');
const { parseManaged, registerFields, startupWindow, watchdogFor, onQuitSignals } = require('./managed.js');
const { PlacementFeed, anyDockPlacement } = require('./dock/placements.js');
const { WindowProbe } = require('./dock/window-probe.js');
const { DockController } = require('./dock/dock.js');
const { sessionCounts, sessionMenuItems } = require('./info.js');

const VERSION = require('../package.json').version;
const APP_NAME = 'herdr+ companion'; // the macOS menu bar, Dock and window titles
const UI_DIR = path.join(__dirname, 'ui');
const ICON_DIR = path.join(__dirname, '..', 'assets', 'icon'); // eye-plus app icon (eye-plus.icns for packaging)
const CHROME_HEIGHT = 56; // strip.css: tabs row 26 + nav row 30; extra rows add to it (reported by the strip)
// The window background behind the bar before it paints: the bar's --chrome colour (light / Dusk dark).
const chromeBackground = () => (nativeTheme.shouldUseDarkColors ? '#1b1c23' : '#efeee9');
// The docked bar is always Dusk (it sits in herdr's dark window); pages keep the system look.
const DOCK_BACKGROUND = '#1b1c23';
const SCHEME = 'herdr-companion';
const SYNC_MIN_INTERVAL_MS = 250; // ≤ 4 syncs/s (contract §2.3)
const SYNC_MAX_BYTES = 256 * 1024; // herdr's cap on a sync's groups
const SYNC_BUDGET_BYTES = Number(process.env.HERDR_COMPANION_SYNC_BUDGET) || SYNC_MAX_BYTES - 8 * 1024;
const REPORT_FLUSH_MS = 250;
const REPORT_BATCH = 50;
const REPORT_RATE = 20; // events/s
const SELECTION_MAX = 8000;
const NOTE_MAX = 2000;
const PROMPT_TIMEOUT_MS = 60000; // an unanswered permission prompt is denied

// ---------- arguments and process switches (before ready) ----------

function argValue(name) {
  const i = process.argv.indexOf(name);
  if (i >= 0 && process.argv[i + 1] && !process.argv[i + 1].startsWith('--')) return process.argv[i + 1];
  const eq = process.argv.find((a) => a.startsWith(name + '='));
  return eq ? eq.slice(name.length + 1) : null;
}
const HEADLESS = process.argv.includes('--headless') || process.env.HERDR_COMPANION_HEADLESS === '1';
// herdr's launcher started it (`--managed --launch-token … [--background]`): see managed.js.
const MANAGED = parseManaged(process.argv, process.env);
const explicitSocket = argValue('--socket') || process.env.HERDR_SOCKET_PATH || null;
const SOCKET = explicitSocket || (process.argv.includes('--default-socket') ? resolveSocketPath([], process.env) : null);
const log = (...a) => console.log(`[companion ${new Date().toISOString().slice(11, 23)}]`, ...a);

if (!SOCKET) {
  console.error(
    'herdr-companion: no herdr socket given. Use --socket <path> (or HERDR_SOCKET_PATH),\n' +
      'or --default-socket to use herdr\'s default API socket.',
  );
  app.exit(2);
}

const profileDir = argValue('--profile-dir');
// Without --profile-dir the profile keeps its old home (productName would move it).
app.setPath('userData', profileDir ? path.resolve(profileDir) : path.join(app.getPath('appData'), 'herdr-companion'));
app.setName(APP_NAME);
// Chromium writes DevToolsActivePort only when user-data-dir is a switch (spike S1).
app.commandLine.appendSwitch('user-data-dir', app.getPath('userData'));
app.commandLine.appendSwitch('remote-debugging-port', '0');
app.commandLine.appendSwitch('remote-debugging-address', '127.0.0.1');
if (HEADLESS && process.platform === 'darwin' && app.dock) app.dock.hide();
else if (process.platform === 'darwin' && app.dock) {
  app.whenReady().then(() => {
    const img = nativeImage.createFromPath(path.join(ICON_DIR, 'eye-plus-512.png'));
    if (!img.isEmpty()) app.dock.setIcon(img);
  });
}

protocol.registerSchemesAsPrivileged([
  { scheme: SCHEME, privileges: { standard: true, secure: true, supportFetchAPI: true } },
]);

// ---------- helpers ----------

function normalizeUrl(text) {
  const t = text.trim();
  if (/^[a-z][a-z0-9+.-]*:/i.test(t) && !/^localhost:\d/i.test(t)) return t;
  if (/^(localhost|\d{1,3}(\.\d{1,3}){3})(:\d+)?(\/|$)/i.test(t)) return 'http://' + t;
  if (!/\s/.test(t) && /\.[a-z]{2,}(:\d+)?(\/|$)/i.test(t)) return 'https://' + t;
  return 'https://duckduckgo.com/?q=' + encodeURIComponent(t);
}

const errOut = (code, message) => ({ error: { code, message } });

async function readDevToolsPort() {
  const file = path.join(app.getPath('userData'), 'DevToolsActivePort');
  for (let i = 0; i < 100; i++) {
    try {
      const port = Number(fs.readFileSync(file, 'utf8').split('\n')[0]);
      if (port > 0) return port;
    } catch {}
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error('DevToolsActivePort did not appear');
}

// ---------- the companion ----------

class Companion {
  constructor() {
    this.store = new GroupStore();
    this.instance = crypto.randomBytes(16).toString('hex');
    this.nonce = crypto.randomBytes(16).toString('hex');
    this.win = null;
    this.control = null;
    this.chromeHeight = CHROME_HEIGHT;
    this.shownTab = null;
    this.flash = null; // transient strip message
    this.restarting = false; // the update chip's restart request is under way
    this.syncTimer = null;
    this.syncing = false;
    this.lastSyncAt = 0;
    this.reports = [];
    this.tokens = REPORT_RATE;
    this.tokensAt = Date.now();
    this.refreshTimer = null;
    this.refreshSerial = serialized(() => this.readHostState(), (e) => log('host_state apply failed', e.message));
    this.focusTimer = null;
    this.selfRaiseUntil = 0; // window focus events before this are our own raise
    this.views = new ViewManager({
      onNav: (id, patch) => this.onNav(id, patch),
      onState: (id) => this.onViewState(id),
      onError: (id, kind, text) => this.onPageError(id, kind, text),
      onOpen: (from, url, disposition, opts) => this.onWindowOpen(from, url, disposition, opts),
      onKey: (input) => this.dockKey(input),
      onPermission: (id, req, done) => this.onPermission(id, req, done),
      onLeave: (id, origin) => this.onLeave(id, origin),
    });
    this.prompts = []; // pending permission prompts {id, view_id, permission, label, origin, dones[], timer}
    this.nextPrompt = 1;
    // Docked mode (launcher design §3.4): a frameless window over herdr's band.
    this.dockWin = null;
    this.docked = false;
    this.dockChromeHeight = CHROME_HEIGHT;
    this.mainWasVisible = false;
    this.startupShow = null;
  }

  async start() {
    const ui = session.fromPartition('herdr-ui');
    ui.protocol.handle(SCHEME, (req) => this.serveUi(req));
    ui.setPermissionRequestHandler((_wc, _p, cb) => cb(false));

    ipcMain.handle('herdr-companion:control', (e, op, a) => {
      if (!this.control || e.sender !== this.control.webContents) return errOut('forbidden', 'not the control page');
      return this.controlCall(op, a);
    });
    ipcMain.handle('herdr-companion:strip', (e, name, a) => {
      const dock = !!(this.dockWindow() && e.sender === this.dockWin.webContents);
      if (!dock && (!this.win || e.sender !== this.win.webContents)) return null;
      return this.stripAction(name, a, { dock });
    });

    this.win = new BrowserWindow({
      show: false,
      width: 1200,
      height: 820,
      minWidth: 480,
      minHeight: 300,
      title: APP_NAME,
      backgroundColor: chromeBackground(),
      icon: path.join(ICON_DIR, 'eye-plus-512.png'),
      paintWhenInitiallyHidden: true,
      titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
      webPreferences: {
        session: ui,
        preload: path.join(__dirname, 'preload', 'strip.js'),
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
      },
    });
    this.views.setHost(this.win);
    this.win.on('resize', () => this.layout());
    const onTheme = () => this.win && this.win.setBackgroundColor(chromeBackground());
    nativeTheme.on('updated', onTheme);
    this.win.on('closed', () => nativeTheme.removeListener('updated', onTheme));
    this.win.on('focus', () => this.onWindowFocus());
    this.win.on('closed', () => {
      this.win = null;
      app.quit();
    });
    // The window's own webContents must load something, or connectOverCDP stalls (spike S1).
    await this.win.loadURL(`${SCHEME}://ui/strip.html`);

    this.control = new WebContentsView({
      webPreferences: {
        session: ui,
        preload: path.join(__dirname, 'preload', 'control.js'),
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
        backgroundThrottling: false,
      },
    });
    this.win.contentView.addChildView(this.control);
    this.control.setBounds({ x: 0, y: 0, width: 1, height: 1 });
    this.control.setVisible(false);
    await this.control.webContents.loadURL(`${SCHEME}://control/`);

    this.installMenu();

    this.port = await readDevToolsPort();
    log(`cdp 127.0.0.1:${this.port} socket ${SOCKET}${HEADLESS ? ' (headless)' : ''}`);

    this.link = new HerdrLink({
      socketPath: SOCKET,
      log,
      register: () => ({
        kind: 'companion',
        instance: this.instance,
        pid: process.pid,
        cdp: { host: '127.0.0.1', port: this.port },
        nonce: this.nonce,
        version: VERSION,
        protocol: 1,
        capabilities: ['views', 'html_view', 'send_page', 'user_actions'],
        ...registerFields(MANAGED),
      }),
    });
    this.link.on('status', (s) => {
      log('status', s.state, s.text);
      this.pushStrip();
    });
    this.link.on('lease', (lease) => this.onLease(lease));
    this.link.on('heartbeat', (lease) => this.onUpdateNote(lease));
    this.link.on('lost', () => this.pushStrip());
    this.link.on('event', (ev) => this.onEvent(ev));
    // Docking: herdr's placements, the terminal windows, and what the dock does with them.
    this.feed = new PlacementFeed({ link: this.link, log });
    const fakeWindows = MANAGED.testHooks ? process.env.HERDR_COMPANION_FAKE_WINDOWS || null : null;
    if (fakeWindows) this.fakeScale = Number(process.env.HERDR_COMPANION_FAKE_SCALE) || 2;
    this.probe = new WindowProbe({ log, fakeFile: fakeWindows });
    this.dock = new DockController({ host: this, feed: this.feed, probe: this.probe, log, headless: HEADLESS, electron: require('electron') });
    this.link.start();
    setInterval(() => this.flushReports(), REPORT_FLUSH_MS).unref();

    // A managed companion started for an agent, by autostart or a crash restart never
    // shows itself; one the user started shows without taking the keyboard.
    // A herdr that keeps an area for the browser gets it docked, not a window: the window
    // waits for herdr's first placements (at most 1.5 s).
    this.startupShow = startupWindow(MANAGED, HEADLESS);
    this.startupTimer = setTimeout(() => this.showStartupWindow(), 1500);
    this.feed.once('placements', () => this.showStartupWindow());
    // managed: herdr gone for good (a crash, kill -9; a handoff's gap is far shorter) quits it
    this.watchdog = watchdogFor(MANAGED, { socketPath: SOCKET, log, onGone: () => app.quit() });
    if (this.watchdog) this.watchdog.start();
    this.layout();
    this.pushStrip();
  }

  async serveUi(req) {
    const u = new URL(req.url);
    let file;
    if (u.host === 'control') file = path.join(UI_DIR, 'control.html');
    else if (u.host === 'ui') file = path.join(UI_DIR, decodeURIComponent(u.pathname));
    if (!file || !within(UI_DIR, path.resolve(file))) return new Response('not found', { status: 404 });
    return net.fetch(pathToFileURL(file).toString());
  }

  installMenu() {
    const run = (k) => () => this.handleKey(k);
    const template = [
      ...(process.platform === 'darwin' ? [{ role: 'appMenu' }] : []),
      {
        label: 'File',
        submenu: [
          { label: 'New Tab', accelerator: 'CmdOrCtrl+T', click: run('t') },
          { label: 'Close Tab', accelerator: 'CmdOrCtrl+W', click: run('w') },
          { label: 'Open Location…', accelerator: 'CmdOrCtrl+L', click: run('l') },
          { type: 'separator' },
          { label: 'Send Page to Agent…', accelerator: 'CmdOrCtrl+Shift+S', click: run('send') },
        ],
      },
      { role: 'editMenu' },
      {
        label: 'View',
        submenu: [
          { label: 'Reload', accelerator: 'CmdOrCtrl+R', click: run('r') },
          { label: 'Back', accelerator: 'CmdOrCtrl+[', click: run('[') },
          { label: 'Forward', accelerator: 'CmdOrCtrl+]', click: run(']') },
          { type: 'separator' },
          { label: 'Go to herdr Tab', click: run('go') },
        ],
      },
    ];
    Menu.setApplicationMenu(Menu.buildFromTemplate(template));
  }

  // ---------- herdr link ----------

  async onLease(lease) {
    log('lease', lease.lease_id, lease.state, 'features', (lease.server_features || []).join(','));
    this.features = new Set(lease.server_features || []);
    this.onUpdateNote(lease);
    this.feed.reset();
    this.feed.start();
    if (!this.features.has('placements')) this.showStartupWindow();
    this.store.resetLease();
    await this.refreshState();
    if (this.link) this.link.releaseEvents();
    this.scheduleSync(0);
  }

  // herdr installed newer app files than this process runs (managed): the bar's chip.
  onUpdateNote(lease) {
    const update = (lease && lease.update) || null;
    if (update !== this.update) {
      this.update = update;
      this.pushStrip();
    }
  }

  // One host_state read at a time, in order (serialized): each request runs on its own
  // connection, so overlapping reads could answer out of order and an older full replace
  // would drop a newer group (and destroy its views). A caller still gets a read that
  // started after its call.
  refreshState() {
    return this.refreshSerial();
  }

  async readHostState() {
    let res;
    try {
      res = await this.link.call('browser.host_state', {});
    } catch (e) {
      log('host_state failed', e.code || e.message);
      return;
    }
    const prevFocus = this.store.focusedTabId;
    const out = this.store.applyHostState(res.state || {});
    for (const g of out.dropped) this.destroyGroupViews(g);
    if (out.moved.length) log('groups moved', JSON.stringify(out.moved));
    if (this.shownTab && !this.store.tabs.has(this.shownTab)) this.shownTab = null;
    // Re-show only when herdr focus moved (or nothing valid is shown): a page an agent raised
    // with focus() stays up across unrelated tab/workspace events.
    const focus = this.store.focusedTabId;
    // Docked, the dock's client decides which group shows (its herdr tab).
    if (this.docked && this.dockTab && this.store.tabs.has(this.dockTab)) {
      if (this.shownTab !== this.dockTab) await this.showGroup(this.dockTab, { reason: 'follow' });
      else {
        this.layout();
        this.pushStrip();
      }
    } else if (focus && (focus !== prevFocus || !this.shownTab)) await this.showGroup(focus, { reason: 'follow', focusMoved: focus !== prevFocus });
    else {
      this.layout();
      this.pushStrip();
    }
    this.scheduleSync();
  }

  refreshSoon(ms) {
    clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => this.refreshState(), ms);
  }

  onEvent(ev) {
    const d = ev.data || {};
    switch (ev.event) {
      case 'tab.focused':
        clearTimeout(this.focusTimer);
        this.focusTimer = setTimeout(() => {
          if (!this.store.tabs.has(d.tab_id)) return this.refreshSoon(0);
          const moved = this.store.focusedTabId !== d.tab_id;
          this.store.focusedTabId = d.tab_id;
          // Docked, the dock's client decides (its placement names its herdr tab).
          if (this.docked && this.dockTab) return;
          this.showGroup(d.tab_id, { reason: 'follow', focusMoved: moved });
        }, 50);
        break;
      case 'tab.renamed': {
        const t = this.store.tabs.get(d.tab_id);
        if (t) t.label = d.label;
        if (d.tab_id === this.shownTab) this.pushStrip();
        break;
      }
      case 'tab.closed':
        // A cross-workspace move may arrive as closed + created: re-read after a grace period.
        this.refreshSoon(1000);
        break;
      default:
        // workspace.focused / tab.created / tab.moved / workspace.*: re-read is the safety net.
        this.refreshSoon(100);
    }
  }

  // ---------- groups and views ----------

  shownGroup() {
    return this.shownTab ? this.store.group(this.shownTab) : null;
  }

  activeEntry(g = this.shownGroup()) {
    return g ? g.entries.find((e) => e.view_id === g.active) || null : null;
  }

  async ensureLive(entry, tabId) {
    if (this.views.has(entry.view_id)) return this.views.get(entry.view_id).ready;
    const p = this.views.create(entry, this.win);
    this.layout();
    const target = await p;
    entry.target = target;
    entry.placeholder = false;
    this.store.markDirty(tabId);
    this.scheduleSync();
    return target;
  }

  // reason 'follow' (herdr focus) | 'agent'. `focusMoved`: herdr focus just moved to this tab,
  // so the user is looking at it: report `shown` (clears needs-you) even when the companion
  // already showed this group (an agent's focus() switched to it first).
  async showGroup(tabId, { reason, focusMoved = false } = {}) {
    const changed = this.shownTab !== tabId;
    this.shownTab = tabId;
    const g = this.store.group(tabId);
    const e = this.activeEntry(g);
    if (e) {
      e.attention = false;
      const p = this.ensureLive(e, tabId);
      this.layout();
      this.pushStrip();
      await p;
    }
    this.layout();
    this.pushStrip();
    if ((changed || focusMoved) && reason === 'follow' && e) this.report('user_action', 'shown', e, g);
  }

  // The user brought the window up: they see the shown group (needs-you clears). A raise
  // the companion did itself for an agent's focus() is not the user looking.
  onWindowFocus() {
    if (Date.now() < this.selfRaiseUntil) return;
    const g = this.shownGroup();
    const e = this.activeEntry(g);
    if (!e) return;
    e.attention = false;
    this.report('user_action', 'shown', e, g);
    this.pushStrip();
  }

  layout() {
    const host = this.docked && this.dockWindow() ? this.dockWin : this.win;
    if (!host) return;
    const [w, h] = host.getContentSize();
    const top = host === this.dockWin ? this.dockChromeHeight : this.chromeHeight;
    const e = this.activeEntry();
    this.views.layout(e ? e.view_id : null, { x: 0, y: top, width: w, height: Math.max(0, h - top) });
    const title = this.windowTitle();
    if (this.win) this.win.setTitle(title);
    if (this.dockWindow()) this.dockWin.setTitle(title);
  }

  // `herdr+ companion — <label> (3)`: the shown session and its page count.
  windowTitle() {
    if (!this.shownTab) return APP_NAME;
    const label = this.store.label(this.shownTab);
    const g = this.shownGroup();
    const n = g ? g.entries.length : 0;
    return `${APP_NAME} — ${label}${n ? ` (${n})` : ''}`;
  }

  // ---------- docked mode (launcher design §3.4; dock/dock.js decides) ----------

  dockWindow() {
    return this.dockWin && !this.dockWin.isDestroyed() ? this.dockWin : null;
  }

  ensureDockWindow() {
    if (this.dockWindow()) return this.dockWin;
    const ui = session.fromPartition('herdr-ui');
    const win = new BrowserWindow({
      show: false,
      frame: false,
      hasShadow: false,
      resizable: false,
      movable: false,
      minimizable: false,
      maximizable: false,
      fullscreenable: false,
      skipTaskbar: true,
      hiddenInMissionControl: true,
      acceptFirstMouse: true,
      width: 600,
      height: 400,
      title: APP_NAME,
      backgroundColor: DOCK_BACKGROUND,
      paintWhenInitiallyHidden: true,
      webPreferences: {
        session: ui,
        preload: path.join(__dirname, 'preload', 'strip.js'),
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
      },
    });
    this.dockWin = win;
    win.on('resize', () => this.layout());
    win.on('focus', () => this.onWindowFocus());
    // A click into the terminal orders it front: back above it at once.
    win.on('blur', () => this.dock && this.dock.onBlur());
    win.on('closed', () => {
      if (this.dockWin === win) this.dockWin = null;
    });
    win.webContents.on('before-input-event', (e, input) => {
      if (this.dockKey(input)) e.preventDefault();
    });
    win.loadURL(`${SCHEME}://ui/strip.html?mode=docked`).catch((e) => log('dock strip failed to load', e.message));
    return win;
  }

  // dock.js: herdr keeps an area for the browser: the pages move into the dock window and
  // the window hides (it comes back when docking ends, if it was up).
  enterDocked() {
    const win = this.ensureDockWindow();
    this.docked = true;
    this.mainWasVisible = !!(this.win && this.win.isVisible());
    if (this.win && !HEADLESS) this.win.hide();
    this.views.setHost(win);
    this.layout();
    this.pushStrip();
  }

  leaveDocked() {
    this.docked = false;
    this.dockTab = null;
    if (this.win) this.views.setHost(this.win);
    this.layout();
    this.pushStrip();
    if (this.mainWasVisible && this.win && !HEADLESS) this.win.showInactive();
    // back to following herdr's focus
    const focus = this.store.focusedTabId;
    if (focus && focus !== this.shownTab) this.showGroup(focus, { reason: 'follow' });
  }

  // dock.js: the target placement's herdr tab shows in the dock.
  dockShowsTab(tabId) {
    this.dockTab = tabId;
    if (tabId && tabId !== this.shownTab && this.store.tabs.has(tabId)) this.showGroup(tabId, { reason: 'follow', focusMoved: true });
  }

  // The window waits for herdr's first placements (docked: it never shows).
  showStartupWindow() {
    clearTimeout(this.startupTimer);
    const show = this.startupShow;
    this.startupShow = null;
    if (!show || !this.win || this.docked || anyDockPlacement(this.feed && this.feed.placements)) return;
    if (show === 'inactive') this.win.showInactive();
    else if (show === 'show') this.win.show();
  }

  // Keys while the dock (or one of its pages) has the keyboard: ⌘⎋ gives it back to the
  // terminal; the bar's shortcuts work without the menu bar (the dock is an accessory).
  dockKey(input) {
    if (!this.docked || !input || input.type !== 'keyDown') return false;
    const cmd = process.platform === 'darwin' ? input.meta : input.control;
    if (!cmd || input.alt) return false;
    const key = String(input.key || '').toLowerCase();
    if (key === 'escape') {
      this.returnKeyboard();
      return true;
    }
    const map = { t: 't', w: 'w', l: 'l', r: 'r', '[': '[', ']': ']' };
    if (!input.shift && map[key]) {
      this.handleKey(map[key]);
      return true;
    }
    if (input.shift && key === 's') {
      this.handleKey('send');
      return true;
    }
    return false;
  }

  // The keyboard back to the terminal: hiding the app activates the one before it (the
  // terminal); the dock comes back over it without taking the keyboard.
  returnKeyboard() {
    if (HEADLESS || process.platform !== 'darwin') return;
    app.hide();
    setTimeout(() => {
      const win = this.dockWindow();
      if (win && this.dock && this.dock.shown) {
        win.showInactive();
        this.dock.raise(win);
      }
    }, 60);
  }

  async openEntry(tabId, spec, { background = true } = {}) {
    const e = this.store.addEntry(tabId, spec, { background });
    if (!this.win) return e;
    await this.ensureLive(e, tabId);
    this.layout();
    this.pushStrip();
    return e;
  }

  closeView(viewId) {
    const hit = this.store.removeEntry(viewId);
    if (!hit) return null;
    this.views.destroy(viewId, this.win);
    if (hit.group.tab_id === this.shownTab) {
      const next = this.activeEntry(hit.group);
      if (next) this.ensureLive(next, hit.group.tab_id).catch(() => {});
    }
    this.layout();
    this.pushStrip();
    this.scheduleSync();
    return hit;
  }

  destroyGroupViews(g) {
    for (const e of g.entries) this.views.destroy(e.view_id, this.win);
    if (g.tab_id === this.shownTab) this.shownTab = null;
  }

  selectView(viewId) {
    const hit = this.store.select(viewId);
    if (!hit) return null;
    if (hit.group.tab_id === this.shownTab) {
      this.ensureLive(hit.entry, hit.group.tab_id).catch(() => {});
      this.layout();
      this.pushStrip();
    }
    this.scheduleSync();
    return hit;
  }

  // ---------- view hooks ----------

  onNav(id, patch) {
    const hit = this.store.updateEntry(id, patch);
    if (!hit) return;
    if (hit.group.tab_id === this.shownTab) this.pushStrip();
    this.scheduleSync();
  }

  onViewState(id) {
    const hit = this.store.findView(id);
    if (hit && hit.group.tab_id === this.shownTab) this.pushStrip();
  }

  onPageError(id, kind, text) {
    const hit = this.store.findView(id);
    if (!hit) return;
    this.queueReport({
      kind: 'page_error',
      error: kind,
      text: String(text || '').slice(0, 300),
      ...this.ctx(hit.entry, hit.group),
    });
  }

  // A page opened a tab (window.open, target=_blank; an html view's script navigation:
  // `fromView`, on behalf of whoever opened the view, not a user action).
  onWindowOpen(fromId, url, disposition, { opened_by = 'user', fromView = false } = {}) {
    const hit = this.store.findView(fromId);
    if (!hit) return;
    const background = disposition !== 'foreground-tab' && disposition !== 'new-window';
    this.openEntry(hit.group.tab_id, { url, opened_by }, { background })
      .then((e) => {
        if (!fromView) this.report('user_action', 'new_tab', e, hit.group);
      })
      .catch((err) => this.say('warn', err.message));
  }

  // ---------- permission prompts (web pages: allow once / deny in the strip) ----------

  onPermission(viewId, req, done) {
    const keyOf = (x) => (x.keys || [x.permission]).join(',');
    const same = this.prompts.find(
      (p) => p.view_id === viewId && p.permission === req.permission && p.origin === req.origin && keyOf(p) === keyOf(req),
    );
    if (same) {
      same.dones.push(done);
      return;
    }
    const p = { id: `p${this.nextPrompt++}`, view_id: viewId, ...req, dones: [done] };
    p.timer = setTimeout(() => this.answerPrompt(p.id, false), PROMPT_TIMEOUT_MS);
    this.prompts.push(p);
    log('permission asked', viewId, req.permission, req.origin);
    this.pushStrip();
  }

  answerPrompt(id, allow) {
    const i = this.prompts.findIndex((p) => p.id === id);
    if (i < 0) return false;
    const [p] = this.prompts.splice(i, 1);
    clearTimeout(p.timer);
    for (const d of p.dones) d(!!allow);
    log('permission', allow ? 'allowed once' : 'denied', p.view_id, p.permission, p.origin);
    this.pushStrip();
    return true;
  }

  // The view navigated to `origin` (null: it closed): its other origins' prompts are denied.
  onLeave(viewId, origin) {
    for (const p of [...this.prompts]) if (p.view_id === viewId && p.origin !== origin) this.answerPrompt(p.id, false);
  }

  // ---------- control page API (contract §1.4) ----------

  async controlCall(op, a) {
    try {
      switch (op) {
        case 'nonce':
          return this.nonce;
        case 'open':
          return await this.ctlOpen(a || {});
        case 'select': {
          const hit = this.selectView(a);
          if (!hit) return errOut('view_not_found', `no view ${a}`);
          return { ok: true, view_id: a };
        }
        case 'focus':
          return await this.ctlFocus(a);
        case 'close': {
          const hit = this.closeView(a);
          if (!hit) return errOut('view_not_found', `no view ${a}`);
          return { ok: true, view_id: a };
        }
        case 'show':
          return await this.ctlShow(a || {});
        case 'views':
          return this.listViews();
        case 'raise':
          return this.raise({ user: !!(a && a.user) });
        case 'quit':
          // herdr's stop of the companion it started (managed), or a headless test run
          if (!HEADLESS && !MANAGED.managed) return errOut('refused', 'quit through the control page is for headless or managed runs only');
          setTimeout(() => app.quit(), 20);
          return { ok: true };
        case '__crash':
          if (!MANAGED.testHooks) return errOut('unknown_op', 'unknown control op __crash');
          log('test hook: crashing');
          setTimeout(() => process.exit(70), 20);
          return { ok: true };
        case 'dock_debug':
          if (!MANAGED.testHooks) return errOut('unknown_op', 'unknown control op dock_debug');
          return this.dock ? this.dock.debug() : null;
        case '__hang':
          if (!MANAGED.testHooks) return errOut('unknown_op', 'unknown control op __hang');
          log('test hook: no more heartbeats');
          if (this.link) this.link.hang();
          return { ok: true };
        default:
          return errOut('unknown_op', `unknown control op ${op}`);
      }
    } catch (e) {
      return errOut(e.code || 'internal', e.message);
    }
  }

  async resolveTab(tabId) {
    const known = (t) => t && (this.store.tabs.has(t) || this.store.group(t));
    let t = tabId || this.store.focusedTabId;
    if (known(t)) return t;
    // a herdr tab created moments ago: its tab.created re-read may not have landed yet
    await this.refreshState();
    t = tabId || this.store.focusedTabId;
    return known(t) ? t : null;
  }

  async ctlOpen(a) {
    const tabId = await this.resolveTab(a.tab_id);
    if (!tabId) return errOut('tab_unknown', `herdr tab ${a.tab_id || '(focused)'} is not known to the companion`);
    const background = a.background === undefined ? true : !!a.background;
    const e = await this.openEntry(tabId, { url: a.url || 'about:blank', opened_by: a.opened_by || 'agent' }, { background });
    return { view_id: e.view_id, target_id: e.target, tab_id: tabId };
  }

  // `focus(view_id)` or `focus({view_id, user})`: an agent's focus (the login handoff) selects
  // the page, marks it and orders the window front without taking the keyboard; the user's
  // own (herdr's overlay, `browser focus` from a terminal) activates the window.
  async ctlFocus(a) {
    const viewId = a && typeof a === 'object' ? a.view_id : a;
    const user = !!(a && typeof a === 'object' && a.user);
    const hit = this.store.findView(viewId);
    if (!hit) return errOut('view_not_found', `no view ${viewId}`);
    this.store.select(viewId);
    this.scheduleSync();
    await this.showGroup(hit.group.tab_id, { reason: 'agent' });
    if (!user) hit.entry.attention = true; // the strip dot; cleared when the user selects it or herdr focus shows it
    this.pushStrip();
    this.raise({ user });
    return { ok: true, view_id: viewId };
  }

  // Bring the window up: `user` activates it (the keyboard goes to it); otherwise it is
  // ordered front without activation (no keyboard theft).
  raise({ user = false } = {}) {
    if (HEADLESS || !this.win) {
      log('raise suppressed (headless)');
      return { ok: true, raised: false };
    }
    // Docked: the dock is where it is. The user's focus gives it the keyboard; an agent's
    // only selects the page (◉ and herdr's notice bring the user).
    if (this.docked) {
      const dock = this.dockWindow();
      if (user && dock && this.dock && this.dock.shown) {
        dock.focus();
        if (process.platform === 'darwin') app.focus({ steal: true });
        return { ok: true, raised: true };
      }
      return { ok: true, raised: false };
    }
    if (user) {
      this.win.show();
      this.win.focus();
      if (process.platform === 'darwin') app.focus({ steal: true });
    } else {
      this.selfRaiseUntil = Date.now() + 1000;
      this.win.showInactive();
    }
    return { ok: true, raised: true };
  }

  async ctlShow(a) {
    const file = a.file;
    if (!file || !path.isAbsolute(file)) return errOut('invalid_request', 'file must be an absolute path');
    let st;
    try {
      st = fs.statSync(file);
    } catch {
      return errOut('file_not_found', `${file} does not exist`);
    }
    if (!st.isFile()) return errOut('file_not_found', `${file} is not a file`);
    const tabId = await this.resolveTab(a.tab_id);
    if (!tabId) return errOut('tab_unknown', `herdr tab ${a.tab_id || '(focused)'} is not known to the companion`);
    const root = a.root && path.isAbsolute(a.root) && within(path.resolve(a.root), path.resolve(file)) ? a.root : path.dirname(file);
    const e = await this.openEntry(
      tabId,
      {
        kind: 'html_view',
        file,
        root,
        allow_network: !!a.allow_network,
        url: pathToFileURL(file).toString(),
        title: a.title || path.basename(file),
        title_fixed: !!a.title, // an explicit --title: the page's <title> never replaces it
        opened_by: a.opened_by || 'agent',
      },
      { background: !a.focus },
    );
    if (a.focus) await this.ctlFocus(e.view_id);
    return { view_id: e.view_id, target_id: e.target, tab_id: tabId };
  }

  listViews() {
    const out = [];
    for (const g of this.store.groups.values()) {
      for (const e of g.entries) {
        out.push({
          view_id: e.view_id,
          target_id: e.target,
          tab_id: g.tab_id,
          key: g.key,
          id: e.id,
          kind: e.kind,
          url: e.url,
          title: e.title,
          active: g.active === e.view_id,
          live: this.views.has(e.view_id),
          shown: g.tab_id === this.shownTab && g.active === e.view_id,
        });
      }
    }
    return out;
  }

  // ---------- strip ----------

  handleKey(k) {
    const g = this.shownGroup();
    const e = this.activeEntry(g);
    const wc = e && this.views.has(e.view_id) ? this.views.get(e.view_id).view.webContents : null;
    switch (k) {
      case 't':
        return this.newTab();
      case 'w':
        if (e) {
          this.report('user_action', 'close_tab', e, g);
          this.closeView(e.view_id);
        }
        return;
      case 'l':
        return this.focusUrl();
      case 'r':
        if (wc) {
          wc.reload();
          this.report('user_action', 'reload', e, g);
        }
        return;
      case '[':
        if (wc && wc.navigationHistory.canGoBack()) {
          wc.navigationHistory.goBack();
          this.report('user_action', 'back', e, g);
        }
        return;
      case ']':
        if (wc && wc.navigationHistory.canGoForward()) {
          wc.navigationHistory.goForward();
          this.report('user_action', 'forward', e, g);
        }
        return;
      case 'send': {
        const strip = this.stripWindow();
        return strip && strip.webContents.executeJavaScript("document.getElementById('send').click()", true);
      }
      case 'go':
        return this.goToTab();
      default:
    }
  }

  // The window whose bar the user sees: the dock's while docked.
  stripWindow() {
    return this.docked && this.dockWindow() ? this.dockWin : this.win;
  }

  focusUrl() {
    const strip = this.stripWindow();
    if (!strip || HEADLESS) return;
    strip.webContents.focus();
    strip.webContents.send('strip:focus-url');
  }

  async newTab(url = 'about:blank') {
    if (!this.shownTab) return;
    try {
      const e = await this.openEntry(this.shownTab, { url, opened_by: 'user' }, { background: false });
      this.report('user_action', 'new_tab', e, this.shownGroup());
      if (url === 'about:blank') this.focusUrl();
    } catch (err) {
      this.say('warn', err.message);
    }
  }

  async goToTab() {
    if (!this.shownTab || !this.link) return;
    try {
      await this.link.request('tab.focus', { tab_id: this.shownTab });
    } catch (e) {
      this.say('warn', `could not focus the herdr tab: ${e.message}`);
    }
  }

  async stripAction(name, a, { dock = false } = {}) {
    const g = this.shownGroup();
    const e = this.activeEntry(g);
    const wc = e && this.views.has(e.view_id) ? this.views.get(e.view_id).view.webContents : null;
    switch (name) {
      case 'ready':
        return this.pushStrip();
      case 'chrome-height': {
        const h = Math.max(40, Math.min(200, Number(a) || CHROME_HEIGHT));
        const field = dock ? 'dockChromeHeight' : 'chromeHeight';
        if (h !== this[field]) {
          this[field] = h;
          this.layout();
        }
        return;
      }
      case 'sessions_menu':
        return this.openSessionsMenu(dock);
      case 'select': {
        const hit = this.selectView(a);
        if (hit) {
          hit.entry.attention = false;
          this.report('user_action', 'select', hit.entry, hit.group);
          // the user looks at it: herdr clears the group's needs-you (an agent's focus() on
          // the tab herdr already shows sends no follow `shown`)
          this.report('user_action', 'shown', hit.entry, hit.group);
          this.pushStrip();
        }
        return;
      }
      case 'close': {
        const hit = this.store.findView(a);
        if (hit) {
          this.report('user_action', 'close_tab', hit.entry, hit.group);
          this.closeView(a);
        }
        return;
      }
      case 'navigate': {
        const url = normalizeUrl(String(a || ''));
        if (!this.shownTab) return;
        if (!e || e.kind === 'html_view') return this.newTab(url);
        if (wc) wc.loadURL(url).catch(() => {});
        else {
          this.store.updateEntry(e.view_id, { url }); // a placeholder: its view loads the new url
          this.ensureLive(e, g.tab_id).catch(() => {});
        }
        this.report('user_action', 'navigate', { ...e, url }, g);
        return;
      }
      case 'back':
      case 'forward':
      case 'reload':
        return this.handleKey(name === 'back' ? '[' : name === 'forward' ? ']' : 'r');
      case 'stop':
        if (wc) {
          wc.stop();
          this.report('user_action', 'stop', e, g);
        }
        return;
      case 'new_tab':
        return this.newTab();
      case 'go_to_tab':
        return this.goToTab();
      case 'send_page':
        return this.sendPage((a && a.note) || '');
      case 'restart_update':
        return this.restartForUpdate();
      case 'permission':
        return a && this.answerPrompt(String(a.id), a.allow === true);
      default:
    }
  }

  // The bar's update chip: herdr restarts this process with the installed app files. One
  // request at a time: the chip is disabled while it runs and comes back when it fails, when
  // herdr answers without restarting (a note), or when this process is still here 30 s later.
  async restartForUpdate() {
    if (!this.link || this.restarting) return;
    this.setRestarting(true);
    this.say('ok', `Restarting into ${this.update || 'the new version'}…`, 15000);
    try {
      const res = await this.link.request('browser.companion', { action: 'restart' });
      if (res && res.note) {
        this.setRestarting(false);
        this.say('warn', res.note);
        return;
      }
      setTimeout(() => this.setRestarting(false), 30000).unref();
    } catch (e) {
      this.setRestarting(false);
      this.say('warn', e.code === 'unknown_method' ? 'This herdr cannot restart the companion' : `Restart failed: ${e.message}`);
    }
  }

  setRestarting(on) {
    if (this.restarting === on) return;
    this.restarting = on;
    this.pushStrip();
  }

  stripState({ dock = false } = {}) {
    const g = this.shownGroup();
    const active = this.activeEntry(g);
    const status = this.link ? this.link.status : { state: 'connecting', text: 'starting…' };
    let line = null;
    if (this.flash && this.flash.until > Date.now()) line = { level: this.flash.level, text: this.flash.text };
    else if (status.state !== 'registered') line = { level: 'warn', text: status.text };
    return {
      inset: process.platform === 'darwin' && !dock,
      docked: dock,
      info: this.sessionInfo(),
      session: this.shownTab
        ? {
            tab_id: this.shownTab,
            label: this.store.label(this.shownTab),
            workspace: (this.store.tabs.get(this.shownTab) || {}).workspace_id || null,
          }
        : null,
      tabs: g
        ? g.entries.map((x) => ({
            view_id: x.view_id,
            title: x.title,
            url: x.url,
            kind: x.kind,
            active: x.view_id === g.active,
            needs_you: !!x.attention,
            opened_by: x.opened_by,
            loading: this.views.has(x.view_id) ? this.views.navState(x.view_id).loading : false,
          }))
        : [],
      active: active ? { view_id: active.view_id, url: active.url, title: active.title, kind: active.kind } : null,
      nav: active ? this.views.navState(active.view_id) : {},
      prompt: (() => {
        const p = active && this.prompts.find((x) => x.view_id === active.view_id);
        return p ? { id: p.id, label: p.label, origin: p.origin, permission: p.permission } : null;
      })(),
      attached: status.state === 'registered',
      status: line,
      update: this.update ? { version: this.update, restarting: this.restarting } : null,
    };
  }

  // The bar's count pill: pages in the shown session, pages and sessions in all of herdr.
  sessionInfo() {
    return sessionCounts(this.store, this.shownTab);
  }

  // The pill's menu: one item per herdr tab with pages; choosing one focuses it in herdr
  // (the companion follows, docked or not).
  openSessionsMenu(dock) {
    const items = sessionMenuItems(this.store, this.shownTab);
    const template = items.length
      ? [
          { label: 'Sessions with pages', enabled: false },
          { type: 'separator' },
          ...items.map((it) => ({
            label: it.label,
            sublabel: it.pages,
            type: 'checkbox',
            checked: it.current,
            click: () => {
              if (!this.link) return;
              this.link.request('tab.focus', { tab_id: it.tab_id }).catch((e) => this.say('warn', `could not focus the herdr tab: ${e.message}`));
            },
          })),
        ]
      : [{ label: 'No session has pages yet', enabled: false }];
    const win = dock ? this.dockWindow() : this.win;
    if (!win || HEADLESS) return template.length;
    Menu.buildFromTemplate(template).popup({ window: win });
    return template.length;
  }

  pushStrip() {
    if (this.win && !this.win.isDestroyed()) this.win.webContents.send('strip:state', this.stripState());
    const dock = this.dockWindow();
    if (dock) dock.webContents.send('strip:state', this.stripState({ dock: true }));
  }

  say(level, text, ms = 6000) {
    this.flash = { level, text, until: Date.now() + ms };
    this.pushStrip();
    setTimeout(() => this.pushStrip(), ms + 50).unref();
  }

  // ---------- send page (contract §5.1) ----------

  async sendPage(note) {
    const g = this.shownGroup();
    const e = this.activeEntry(g);
    if (!g || !e) return;
    let selection = '';
    if (this.views.has(e.view_id)) {
      try {
        // Isolated world: page scripts cannot fake the selection getter.
        selection = await this.views
          .get(e.view_id)
          .view.webContents.executeJavaScriptInIsolatedWorld(1001, [{ code: 'String(window.getSelection ? window.getSelection() : "")' }]);
      } catch {}
    }
    const params = {
      tab_id: g.tab_id,
      url: e.url,
      title: e.title,
      selection: String(selection || '').slice(0, SELECTION_MAX),
      note: String(note || '').slice(0, NOTE_MAX),
    };
    try {
      const res = await this.link.call('browser.send_page', params);
      const who = res.to_name || res.to_pane || 'the agent';
      if (res.outcome === 'sent') this.say('ok', `Sent to ${who}`);
      else this.say('ok', `Queued for ${who}${res.reason ? ` (${res.reason})` : ''}`);
      this.report('user_action', 'send_page', e, g);
    } catch (err) {
      const msg =
        err.code === 'no_agent_in_tab'
          ? 'No agent in this herdr tab to send the page to'
          : err.code === 'unknown_method'
            ? 'This herdr cannot receive pages yet'
            : `Send failed: ${err.message}`;
      this.say('warn', msg);
    }
  }

  // ---------- host_sync writer (contract §2.3) ----------

  scheduleSync(delay) {
    if (this.syncTimer || this.syncing) {
      this.syncAgain = true;
      return;
    }
    const wait = Math.max(delay === undefined ? 30 : delay, this.lastSyncAt + SYNC_MIN_INTERVAL_MS - Date.now());
    this.syncTimer = setTimeout(() => {
      this.syncTimer = null;
      this.doSync();
    }, Math.max(0, wait));
  }

  async doSync() {
    if (!this.link || !this.link.lease) return; // a new lease resyncs everything
    // Byte-bounded: the groups left over stay dirty and go in the next sync (its own seq).
    const payload = this.store.takeSync(this.link.lease.lease_id, { maxBytes: SYNC_BUDGET_BYTES });
    if (!payload) return;
    this.syncing = true;
    this.syncAgain = false;
    this.lastSyncAt = Date.now();
    try {
      const { lease_id: _l, ...rest } = payload;
      const res = await this.link.call('browser.host_sync', rest);
      const unknown = this.store.applySynced(res);
      // A tab herdr does not know may have moved (new tab id, same key): re-read after the
      // same grace as tab.closed; host_state follows the key or drops a tab that is gone.
      if (unknown.length) {
        log('sync named unknown tabs', unknown.join(','), '- re-reading host state');
        this.refreshSoon(1000);
      }
    } catch (e) {
      if (e.code !== 'stale_seq') {
        log('host_sync failed', e.code || e.message);
        for (const gr of payload.groups) this.store.markDirty(gr.tab_id);
      }
    } finally {
      this.syncing = false;
    }
    if (this.syncAgain || this.store.dirty.size) {
      this.syncAgain = false;
      if (this.link.lease) this.scheduleSync();
    }
  }

  // ---------- host_report (contract §5.3) ----------

  ctx(entry, group) {
    return { tab_id: group.tab_id, view_id: entry.view_id, target: entry.target || undefined, url: entry.url };
  }

  report(kind, action, entry, group) {
    if (!entry || !group) return;
    this.queueReport({ kind, action, ...this.ctx(entry, group) });
  }

  queueReport(ev) {
    const now = Date.now();
    this.tokens = Math.min(REPORT_RATE, this.tokens + ((now - this.tokensAt) / 1000) * REPORT_RATE);
    this.tokensAt = now;
    // `shown` (the user looks: herdr clears needs-you) is never rate limited; herdr admits
    // it past its own bucket too
    if (ev.action !== 'shown') {
      if (this.tokens < 1) return; // over the rate: dropped here, never an error
      this.tokens -= 1;
    }
    this.reports.push({ ...ev, at_ms: now });
    if (this.reports.length > 200) this.reports.splice(0, this.reports.length - 200);
  }

  async flushReports() {
    if (!this.reports.length || this.reporting || !this.link || !this.link.lease) return;
    if (this.features && !this.features.has('host_report')) {
      this.reports = [];
      return;
    }
    const events = this.reports.splice(0, REPORT_BATCH);
    this.reporting = true;
    try {
      await this.link.call('browser.host_report', { events });
    } catch (e) {
      log('host_report failed', e.code || e.message);
    } finally {
      this.reporting = false;
    }
  }

  async shutdown() {
    if (this.watchdog) this.watchdog.stop();
    if (this.dock) this.dock.stop();
    if (this.feed) this.feed.stop();
    if (this.link) await this.link.stop();
  }
}

// ---------- lifecycle ----------

if (SOCKET) {
  if (!app.requestSingleInstanceLock()) {
    console.error('herdr-companion: another companion already uses this profile');
    app.quit();
  } else {
    const companion = new Companion();
    let quitting = false;
    app.on('before-quit', (e) => {
      if (quitting) return;
      e.preventDefault();
      quitting = true;
      companion.shutdown().finally(() => app.quit());
    });
    app.on('window-all-closed', () => app.quit());
    // herdr's stop (after the control page's quit) and a terminal's ^C: a clean quit that
    // unregisters first (before-quit above)
    onQuitSignals(process, (signal) => {
      log(`${signal}: quitting`);
      app.quit();
    });
    app.on('second-instance', () => {
      if (!HEADLESS && companion.win) {
        companion.win.show();
        companion.win.focus();
      }
    });
    app.whenReady().then(() =>
      companion.start().catch((e) => {
        console.error('herdr-companion failed to start:', e);
        app.quit();
      }),
    );
  }
}
