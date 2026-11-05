// herdr+ companion browser: Electron main process (contract §6).
// One window; a WebContentsView per page; one group of pages per herdr tab; follows herdr focus.
'use strict';
const { app, BrowserWindow, WebContentsView, ipcMain, protocol, session, Menu, net, nativeTheme, nativeImage, clipboard } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { pathToFileURL, fileURLToPath } = require('node:url');
const { GroupStore } = require('./groups.js');
const { ViewManager, within, frameKey } = require('./views.js');
const { DialogBook } = require('./dialogs.js');
const { HerdrLink } = require('./host.js');
const { resolveSocketPath, serialized } = require('./herdr-api.js');
const { parseManaged, registerFields, startupWindow, watchdogFor, onQuitSignals } = require('./managed.js');
const { PlacementFeed, anyDockPlacement } = require('./dock/placements.js');
const { WindowProbe } = require('./dock/window-probe.js');
const { DockController } = require('./dock/dock.js');
const { sessionCounts, sessionMenuItems } = require('./info.js');
const { pageMenuItems, pageMenuActions } = require('./context-menu.js');
const bar = require('./ui/bar-model.js');
const pageApi = require('./page-api.js');

const VERSION = require('../package.json').version;
const APP_NAME = 'herdr+ companion'; // the macOS menu bar, Dock and window titles
const UI_DIR = path.join(__dirname, 'ui');
const ICON_DIR = path.join(__dirname, '..', 'assets', 'icon'); // eye-plus app icon (eye-plus.icns for packaging)
const CHROME_HEIGHT = 30; // strip.css: one 30 px row (two rows of 28 when narrow); extra rows add to it (reported by the strip)
// The window background behind the bar before it paints: the bar's --chrome colour (light / Dusk dark).
const chromeBackground = () => (nativeTheme.shouldUseDarkColors ? '#1b1c23' : '#efeee9');
// The docked bar is always Dusk (it sits in herdr's dark window); pages keep the system look.
const DOCK_BACKGROUND = '#1b1c23';
const SCHEME = 'herdr-companion';
const DIALOG_SWEEP_MS = 1000; // how soon a dialog of a frame that went away is cancelled
const SYNC_MIN_INTERVAL_MS = 250; // ≤ 4 syncs/s (contract §2.3)
const SYNC_MAX_BYTES = 256 * 1024; // herdr's cap on a sync's groups
const SYNC_BUDGET_BYTES = Number(process.env.HERDR_COMPANION_SYNC_BUDGET) || SYNC_MAX_BYTES - 8 * 1024;
const REPORT_FLUSH_MS = 250;
const REPORT_BATCH = 50;
const REPORT_RATE = 20; // events/s
const SELECTION_MAX = 8000;
const NOTE_MAX = 2000;
const PROMPT_TIMEOUT_MS = 60000; // an unanswered permission prompt is denied

// The bar's pins per surface (docked, full, floating), in the profile; defaults in bar-model.js.
const barPinsFile = () => path.join(app.getPath('userData'), 'bar.json');
function loadBarPins() {
  try {
    const doc = JSON.parse(fs.readFileSync(barPinsFile(), 'utf8'));
    const out = {};
    for (const mode of Object.keys(bar.PIN_DEFAULTS)) if (doc && doc.pinned && typeof doc.pinned[mode] === 'boolean') out[mode] = doc.pinned[mode];
    return out;
  } catch (e) {
    // no file yet: the defaults; anything else is worth a line in the log
    if (e.code !== 'ENOENT') log('bar pins unreadable', e.message);
    return {};
  }
}
function saveBarPins(pins) {
  try {
    fs.writeFileSync(barPinsFile(), JSON.stringify({ version: 1, pinned: pins }) + '\n');
  } catch (e) {
    log('bar pins not saved', e.message);
  }
}

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
      onContextMenu: (id, params) => this.openPageMenu(id, params),
      onDialogsEnd: (id, frame) => this.dialogs.dismissView(id, { frame }),
      onRaise: (host) => this.raiseDockBar(host),
      // page API: views get window.herdr only while herdr advertises page messages
      pageApiOn: () => !!(this.features && this.features.has(pageApi.FEATURE)),
    });
    this.pageRates = new Map(); // view_id -> RateLimit (page API)
    // pages' alert / confirm / prompt (src/dialogs.js), shown in the dialog card over the page
    this.dialogs = new DialogBook({ onChange: () => this.onDialogsChanged() });
    this.dialogView = null;
    this.dialogShown = null; // {id, host, focus} of the card on screen
    this.dialogHeight = 150; // the card's height (it reports it)
    this.dialogSpace = 0; // the height the card may take (below the bar)
    this.dialogSweep = null; // the frame check while a dialog is open
    this.prompts = []; // pending permission prompts {id, view_id, permission, label, origin, dones[], timer}
    this.nextPrompt = 1;
    // Docked mode (launcher design §3.4): a frameless window over herdr's band.
    this.dockWin = null;
    this.docked = false;
    this.dockPlacement = null; // {id, kind} of the placement the dock covers (its bar's window actions)
    this.dockBar = null; // the dock's bar: a view above its pages (it floats over them while unpinned)
    // The bar's auto-hide: the pin per surface (saved), what each bar reports (its height and
    // whether it shows), a counter that makes it show for a moment, the pill menu being open.
    this.barPins = loadBarPins();
    this.barReport = { win: { height: CHROME_HEIGHT, shown: true }, dock: { height: CHROME_HEIGHT, shown: true } };
    this.barPulse = 0;
    this.barMenuOpen = false;
    this.lastActive = null; // {view_id, url, loading} of the shown page (its changes make the bar show)
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
    ipcMain.on('herdr-companion:dialog', (e, req) => this.onPageDialog(e, req));
    ipcMain.handle('herdr-companion:page-message', (e, req) => this.onPageMessage(e, req));
    ipcMain.on('herdr-companion:dialog-ui', (e, kind, a) => {
      if (this.dialogView && e.sender === this.dialogView.webContents) this.onDialogUi(kind, a || {});
    });
    ipcMain.handle('herdr-companion:strip', (e, name, a) => {
      const dock = !!(this.dockBarView() && e.sender === this.dockBar.webContents);
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
    this.createDialogView();

    this.installMenu();

    this.port = await readDevToolsPort();
    log(`cdp 127.0.0.1:${this.port} socket ${SOCKET}${HEADLESS ? ' (headless)' : ''}`);

    this.link = new HerdrLink({
      socketPath: SOCKET,
      log,
      register: () => {
        // herdr attaches its sidecar before it answers, and the attach stalls on a page blocked
        // in its dialog call (its renderer answers no CDP until then): open dialogs are
        // cancelled first, as a navigation would (null / false)
        const n = this.dialogs.dismissAll();
        if (n) log('dialogs cancelled before herdr attaches', n);
        return this.registerParams();
      },
    });
    this.registerParams = () => ({
      kind: 'companion',
      instance: this.instance,
      pid: process.pid,
      cdp: { host: '127.0.0.1', port: this.port },
      nonce: this.nonce,
      version: VERSION,
      protocol: 1,
      capabilities: ['views', 'html_view', 'send_page', 'user_actions'],
      ...registerFields(MANAGED),
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
          { label: 'Pin the Bar', accelerator: 'CmdOrCtrl+Shift+.', click: run('pin') },
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
    this.loadAgentPages(out.created);
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

  // Restored pages are placeholders until shown, except the agents' own (`agent:<pane>`): an
  // agent works on its page without looking at it, so after a restart its view loads at once,
  // hidden, one at a time; herdr's sync then gives the agent its page back under the same
  // herdr tab id. (An exception to the lazy restore, kept to the agents' pages.)
  loadAgentPages(tabIds) {
    const todo = [];
    for (const tabId of tabIds || []) {
      const g = this.store.group(tabId);
      if (!g) continue;
      for (const e of g.entries) if (e.placeholder && /^agent:/.test(e.opened_by || '')) todo.push([e, tabId]);
    }
    if (!todo.length) return;
    (async () => {
      for (const [e, tabId] of todo) {
        if (!this.store.findView(e.view_id)) continue; // closed meanwhile
        await this.ensureLive(e, tabId).catch((err) => log('restore load failed', e.view_id, err && err.message));
      }
    })();
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
    this.focusDialog();
    if (Date.now() < this.selfRaiseUntil) return;
    const g = this.shownGroup();
    const e = this.activeEntry(g);
    if (!e) return;
    e.attention = false;
    this.report('user_action', 'shown', e, g);
    this.pushStrip();
  }

  // Where the page starts in `host` and how tall the bar's own area is (the dock's bar view).
  // Pinned (or no page to cover): the bar is part of the layout and the page starts below it.
  // Unpinned and docked: the page takes the whole band and the bar floats over it, only its
  // 6 px hit zone while hidden. Unpinned in the floating window (its bar is the window's own
  // page, under the views): the page starts below the hit zone, below the bar while it shows.
  barGeometry(host) {
    const dock = host === this.dockWin;
    const [, h] = host.getContentSize();
    const r = dock ? this.barReport.dock : this.barReport.win;
    const height = Math.max(0, Math.min(200, Number(r.height) || 0));
    const placement = dock ? this.dockPlacement : null;
    const mode = bar.surface(dock, placement ? placement.kind : 'side');
    const pinned = bar.pinnedFor(this.barPins, mode);
    const page = !!this.activeEntry();
    if (pinned || !page) return { mode, pinned, top: height, view: page ? height : h, overlay: false };
    if (dock) return { mode, pinned, top: 0, view: r.shown ? height : bar.ZONE_PX, overlay: true };
    return { mode, pinned, top: r.shown ? height : bar.ZONE_PX, view: r.shown ? height : bar.ZONE_PX, overlay: false };
  }

  layout() {
    const host = this.docked && this.dockWindow() ? this.dockWin : this.win;
    if (!host) return;
    const [w, h] = host.getContentSize();
    const g = this.barGeometry(host);
    const top = g.top;
    this.lastBar = { host: host === this.dockWin ? 'dock' : 'win', ...g };
    if (host === this.dockWin && this.dockBarView()) this.dockBar.setBounds({ x: 0, y: 0, width: w, height: Math.max(1, Math.min(h, g.view)) });
    // the floating window's traffic lights come and go with its bar
    if (host === this.win && process.platform === 'darwin' && typeof this.win.setWindowButtonVisibility === 'function') {
      this.win.setWindowButtonVisibility(g.pinned || !!this.barReport.win.shown || !this.activeEntry());
    }
    const e = this.activeEntry();
    this.views.layout(e ? e.view_id : null, { x: 0, y: top, width: w, height: Math.max(0, h - top) });
    this.layoutDialog();
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
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
      },
    });
    this.dockWin = win;
    // The bar is a view above the pages (unpinned it floats over the page; its bounds shrink to
    // the 6 px hit zone while hidden, so clicks below reach the page).
    const barView = new WebContentsView({
      webPreferences: {
        session: ui,
        preload: path.join(__dirname, 'preload', 'strip.js'),
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
      },
    });
    barView.setBackgroundColor('#00000000');
    win.contentView.addChildView(barView);
    barView.setBounds({ x: 0, y: 0, width: 600, height: CHROME_HEIGHT });
    this.dockBar = barView;
    barView.webContents.on('before-input-event', (e, input) => {
      if (this.dockKey(input)) e.preventDefault();
    });
    win.on('resize', () => this.layout());
    win.on('focus', () => {
      this.onWindowFocus();
      // a hover over a dock that is not the key window may not reach its bar: the bar also
      // shows for a moment when the user brings the dock up (not the companion's own raise
      // for an agent's focus)
      if (Date.now() >= this.selfRaiseUntil) this.pulseBar();
    });
    // The keyboard went elsewhere (a click into the terminal): the dock's level and order again.
    win.on('blur', () => this.dock && this.dock.onBlur());
    win.on('closed', () => {
      if (this.dockWin === win) this.dockWin = null;
      if (this.dockBar === barView) this.dockBar = null;
    });
    win.webContents.on('before-input-event', (e, input) => {
      if (this.dockKey(input)) e.preventDefault();
    });
    // the window's own page is only the background (the bar and the pages are views over it)
    win.loadURL('about:blank').catch(() => {});
    barView.webContents.loadURL(`${SCHEME}://ui/strip.html?mode=docked`).catch((e) => log('dock strip failed to load', e.message));
    return win;
  }

  dockBarView() {
    return this.dockBar && !this.dockBar.webContents.isDestroyed() && this.dockWindow() ? this.dockBar : null;
  }

  // Adding a view again moves it to the top: the dock's bar above a page added after it.
  raiseDockBar(host) {
    if (host && host === this.dockWindow() && this.dockBarView()) host.contentView.addChildView(this.dockBar);
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
    this.dockPlacement = null;
    if (this.win) this.views.setHost(this.win);
    this.layout();
    this.pushStrip();
    if (this.mainWasVisible && this.win && !HEADLESS) this.win.showInactive();
    // back to following herdr's focus
    const focus = this.store.focusedTabId;
    if (focus && focus !== this.shownTab) this.showGroup(focus, { reason: 'follow' });
  }

  // dock.js: the target placement's herdr tab shows in the dock; its id and kind (side or
  // full) pick the bar's window actions.
  dockShowsTab(tabId, placement = null) {
    const next = placement && placement.id ? { id: placement.id, kind: placement.kind === 'full' ? 'full' : 'side', actions: !!placement.actions } : null;
    const was = this.dockPlacement;
    this.dockPlacement = next;
    if (!was !== !next || (was && next && (was.id !== next.id || was.kind !== next.kind || was.actions !== next.actions))) this.pushStrip();
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
    if (input.shift && (input.code === 'Period' || key === '>' || key === '.')) {
      this.handleKey('pin');
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
    this.pageRates.delete(viewId);
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
    if (hit.group.tab_id === this.shownTab) {
      this.notePageChange();
      this.pushStrip();
    }
    this.scheduleSync();
  }

  onViewState(id) {
    const hit = this.store.findView(id);
    if (hit && hit.group.tab_id === this.shownTab) {
      this.notePageChange();
      this.pushStrip();
    }
  }

  // The shown page moved to another URL, finished loading, or another page shows: the hidden
  // bar shows for a moment (the URL matters for trust when it changes). Counted only; the
  // caller pushes the strip.
  notePageChange() {
    const e = this.activeEntry();
    const now = e ? { view_id: e.view_id, url: e.url, loading: this.views.has(e.view_id) ? this.views.navState(e.view_id).loading : false } : null;
    const was = this.lastActive;
    this.lastActive = now;
    if (!was || !now) return;
    if (was.view_id !== now.view_id || was.url !== now.url || (was.loading && !now.loading)) this.barPulse++;
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

  // ---------- page dialogs (alert / confirm / prompt; src/dialogs.js) ----------

  // The card: one transparent view above the pages (it moves with them into the dock).
  createDialogView() {
    const view = new WebContentsView({
      webPreferences: {
        session: session.fromPartition('herdr-ui'),
        preload: path.join(__dirname, 'preload', 'dialog.js'),
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
        backgroundThrottling: false,
      },
    });
    view.setBackgroundColor('#00000000');
    view.setVisible(false);
    view.webContents.on('before-input-event', (e, input) => {
      if (this.dockKey(input)) e.preventDefault();
    });
    this.dialogView = view;
    this.views.addOverlay(view, this.win);
    view.webContents.loadURL(`${SCHEME}://ui/dialog.html`).catch((e) => log('dialog card failed to load', e.message));
  }

  // A page's preload asked (sync IPC): the page waits for e.returnValue.
  onPageDialog(e, req) {
    const viewId = this.views.viewIdOf(e.sender);
    let frame = null;
    let origin = '';
    let embedded = false;
    const ancestors = []; // a navigation of any of them cancels it too
    let alive = null;
    try {
      const f = e.senderFrame;
      frame = frameKey(f);
      origin = f ? f.origin : '';
      embedded = !!(f && f.parent);
      for (let p = f && f.parent; p; p = p.parent) {
        const k = frameKey(p);
        if (k) ancestors.push(k);
      }
      // gone without navigating: an iframe removed, its process crashed, its document replaced
      if (f) alive = () => !f.isDestroyed() && !f.detached;
    } catch {}
    let answered = false;
    const reply = (value) => {
      if (answered) return;
      answered = true;
      try {
        e.returnValue = value;
      } catch {}
    };
    if (!viewId || !this.store.findView(viewId)) return reply(null);
    const d = this.dialogs.open(viewId, req, reply, { frame, ancestors, alive, info: { origin: origin && origin !== 'null' ? origin : '', embedded } });
    if (!d) return;
    log('dialog', d.id, d.type, viewId);
    const hit = this.store.findView(viewId);
    // a page of another session (or not the shown one) asks: the strip marks it
    if (hit && !(hit.group.tab_id === this.shownTab && hit.group.active === viewId)) {
      hit.entry.attention = true;
      this.pushStrip();
    }
  }

  onDialogsChanged() {
    this.layoutDialog();
    this.pushDialogs();
    // while any is open: a frame that goes away without navigating fires nothing, so check
    if (this.dialogs.size && !this.dialogSweep) this.dialogSweep = setInterval(() => this.dialogs.prune(), DIALOG_SWEEP_MS);
    else if (!this.dialogs.size && this.dialogSweep) {
      clearInterval(this.dialogSweep);
      this.dialogSweep = null;
    }
  }

  // herdr's sidecar follows the open dialogs (its dialog events, `browser dialog`): the whole
  // list each time, to a binding it put on the control page (absent: nobody listens).
  dialogState() {
    return { seq: this.dialogs.seq, dialogs: this.dialogs.list((v) => (this.views.has(v) ? this.views.get(v).target : null)) };
  }

  pushDialogs() {
    const wc = this.control && this.control.webContents;
    if (!wc || wc.isDestroyed()) return;
    const js = `globalThis.__herdrDialogs && globalThis.__herdrDialogs(${JSON.stringify(this.dialogState())})`;
    wc.executeJavaScript(js, false).catch(() => {});
  }

  // Shows the card over the shown page when it has a dialog (layout() calls it too).
  layoutDialog() {
    const view = this.dialogView;
    if (!view) return;
    const host = this.docked && this.dockWindow() ? this.dockWin : this.win;
    const e = this.activeEntry();
    const d = host && e ? this.dialogs.top(e.view_id) : null;
    if (!d) {
      view.setVisible(false);
      if (this.dialogShown) {
        const hadFocus = view.webContents.isFocused();
        this.dialogShown = null;
        view.webContents.send('dialog:show', null);
        // the keyboard goes back to the page
        if (hadFocus && e && this.views.has(e.view_id)) this.views.get(e.view_id).view.webContents.focus();
      }
      return;
    }
    const [w, h] = host.getContentSize();
    // below the bar, also while it floats over the page
    const g = this.barGeometry(host);
    const top = g.overlay ? Math.max(g.top, (host === this.dockWin ? this.barReport.dock : this.barReport.win).height) : g.top;
    const width = Math.max(200, Math.min(480, w - 24));
    const space = Math.max(60, h - top); // the card's message scrolls to fit it (ui/dialog.js)
    const height = Math.max(60, Math.min(this.dialogHeight, space));
    view.setBounds({ x: Math.round((w - width) / 2), y: top, width, height });
    const fresh = !this.dialogShown || this.dialogShown.id !== d.id || this.dialogShown.host !== host;
    const focus = !HEADLESS && host.isFocused();
    if (fresh) {
      this.dialogShown = { id: d.id, host };
      this.dialogSpace = space;
      view.webContents.send('dialog:show', {
        id: d.id,
        type: d.type,
        message: d.message,
        value: d.value,
        // a frame's never reads as the page's own (an opaque origin: sandboxed, data:, srcdoc)
        origin: d.embedded ? (d.origin ? `An embedded page at ${d.origin}` : 'An embedded page') : d.origin,
        docked: host === this.dockWin,
        space,
        focus,
      });
      this.views.raiseOverlays(host);
    } else if (space !== this.dialogSpace) {
      this.dialogSpace = space;
      view.webContents.send('dialog:space', space);
    }
    view.setVisible(true);
    if (fresh && focus) view.webContents.focus();
  }

  // The keyboard to the card (the window came up with a dialog on screen).
  focusDialog() {
    const view = this.dialogView;
    if (!view || !this.dialogShown || HEADLESS) return;
    view.webContents.focus();
    view.webContents.send('dialog:focus');
  }

  onDialogUi(kind, a) {
    switch (kind) {
      case 'ready':
        this.dialogShown = null;
        return this.layoutDialog();
      case 'size': {
        const h = Math.max(0, Math.min(2000, Number(a.height) || 0));
        if (h && h !== this.dialogHeight) {
          this.dialogHeight = h;
          this.layoutDialog();
        }
        return;
      }
      case 'input': {
        const v = this.dialogs.viewOf(String(a.id));
        if (v) this.dialogs.setValue(v, String(a.id), a.value);
        return;
      }
      case 'answer': {
        const v = this.dialogs.viewOf(String(a.id));
        if (!v) return;
        const d = this.dialogs.answer(v, { id: String(a.id), accept: !!a.accept, text: typeof a.value === 'string' ? a.value : undefined });
        if (d) log('dialog answered', d.id, a.accept ? 'ok' : 'cancel');
        return;
      }
      default:
    }
  }

  // Before a navigation or a close the user asked for: the shown dialogs are cancelled (the
  // page's blocked call returns), as a browser does.
  endDialogs(viewId) {
    if (viewId) this.dialogs.dismissView(viewId);
  }

  // herdr's sidecar answers a dialog (`browser dialog accept|dismiss [text]`).
  ctlDialog(a) {
    const viewId = a && a.view_id ? String(a.view_id) : null;
    const d = viewId && this.dialogs.top(viewId);
    if (!d || (a.id && a.id !== d.id)) return errOut('no_dialog', `no dialog is open on view ${viewId}`);
    this.dialogs.answer(viewId, { id: d.id, accept: !!a.accept, text: typeof a.text === 'string' ? a.text : undefined });
    log('dialog answered by herdr', d.id, a.accept ? 'accept' : 'dismiss');
    return { ok: true, view_id: viewId, id: d.id, type: d.type, message: d.message };
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
        case 'dialogs':
          return this.dialogState();
        case 'dialog':
          return this.ctlDialog(a || {});
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
        case 'page_menu': {
          // the last page context menu: its items and whether it is open; `close` closes it
          if (!MANAGED.testHooks) return errOut('unknown_op', 'unknown control op page_menu');
          const m = this.pageMenu;
          if (!m) return null;
          if (a && a.close && m.open) m.menu.closePopup();
          return { view_id: m.view_id, items: m.items, open: m.open };
        }
        case 'bar_debug':
          // the last bar layout: which window, its surface and pin, where the page starts and
          // how tall the bar's own area is, what the bars reported, the saved pins
          if (!MANAGED.testHooks) return errOut('unknown_op', 'unknown control op bar_debug');
          return { layout: this.lastBar || null, report: this.barReport, pins: this.barPins, dockBar: this.dockBarView() ? this.dockBar.getBounds() : null };
        case 'sessions_menu':
          // the pill menu's items as last built (headless: built, never shown)
          if (!MANAGED.testHooks) return errOut('unknown_op', 'unknown control op sessions_menu');
          return this.sessionsMenu || null;
        case 'strip_preview':
          // the floating window's bar rendered as another surface would show it ({dock, kind,
          // features}; null: its own again), through the same state code
          if (!MANAGED.testHooks) return errOut('unknown_op', 'unknown control op strip_preview');
          this.stripPreview = a && typeof a === 'object' ? a : null;
          this.pushStrip();
          return { ok: true };
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
  // Docked over another herdr tab, the dock keeps that tab's pages (it would switch back on
  // its next decision): the page is selected in its own group and marked; the user's own
  // focus brings its herdr tab forward instead, and the dock follows where that tab has it.
  async ctlFocus(a) {
    const viewId = a && typeof a === 'object' ? a.view_id : a;
    const user = !!(a && typeof a === 'object' && a.user);
    const hit = this.store.findView(viewId);
    if (!hit) return errOut('view_not_found', `no view ${viewId}`);
    this.store.select(viewId);
    this.scheduleSync();
    const tabId = hit.group.tab_id;
    if (this.docked && this.dockTab && tabId !== this.dockTab) {
      if (user) {
        if (this.link) this.link.request('tab.focus', { tab_id: tabId }).catch((e) => this.say('warn', `could not focus the herdr tab: ${e.message}`));
      } else hit.entry.attention = true; // herdr's ◉ and notice bring the user
      this.pushStrip();
      return { ok: true, view_id: viewId };
    }
    await this.showGroup(tabId, { reason: 'agent' });
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
        // page API: only while herdr advertises it, never with network
        page_api: !!a.page_api && !a.allow_network && !!(this.features && this.features.has(pageApi.FEATURE)),
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
        this.pulseBar();
        return this.newTab();
      case 'w':
        if (e) {
          this.report('user_action', 'close_tab', e, g);
          this.closeView(e.view_id);
          this.pulseBar();
        }
        return;
      case 'l':
        return this.focusUrl();
      case 'r':
        if (wc) {
          this.endDialogs(e.view_id);
          wc.reload();
          this.report('user_action', 'reload', e, g);
        }
        return;
      case '[':
        if (wc && wc.navigationHistory.canGoBack()) {
          this.endDialogs(e.view_id);
          wc.navigationHistory.goBack();
          this.report('user_action', 'back', e, g);
        }
        return;
      case ']':
        if (wc && wc.navigationHistory.canGoForward()) {
          this.endDialogs(e.view_id);
          wc.navigationHistory.goForward();
          this.report('user_action', 'forward', e, g);
        }
        return;
      case 'send': {
        const strip = this.stripContents();
        return strip && strip.executeJavaScript("document.getElementById('send').click()", true);
      }
      case 'go':
        return this.goToTab();
      case 'pin': {
        // ⌘⇧. : the shown bar's pin
        const dock = !!(this.docked && this.dockBarView());
        const p = dock ? this.dockPlacement : null;
        const mode = bar.surface(dock, p ? p.kind : 'side');
        return this.setBarPin(mode, !bar.pinnedFor(this.barPins, mode));
      }
      default:
    }
  }

  // The window whose bar the user sees: the dock's while docked.
  stripWindow() {
    return this.docked && this.dockWindow() ? this.dockWin : this.win;
  }

  // The page of the bar the user sees: the dock's bar view while docked, else the window's own.
  stripContents() {
    if (this.docked && this.dockBarView()) return this.dockBar.webContents;
    return this.win && !this.win.isDestroyed() ? this.win.webContents : null;
  }

  // ⌘L: the bar shows (the URL field holds it open) and the field takes the keyboard.
  focusUrl() {
    const strip = this.stripContents();
    if (!strip || HEADLESS) return;
    strip.focus();
    strip.send('strip:focus-url');
  }

  // The bar shows for a moment (a navigation, a new or closed tab, the dock taking the keyboard).
  pulseBar() {
    this.barPulse++;
    this.pushStrip();
  }

  // The pin of surface `mode`, saved with the profile.
  setBarPin(mode, pinned) {
    if (!Object.prototype.hasOwnProperty.call(bar.PIN_DEFAULTS, mode)) return;
    this.barPins = { ...this.barPins, [mode]: !!pinned };
    saveBarPins(this.barPins);
    this.layout();
    this.pushStrip();
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
      case 'bar': {
        // the bar's height (30 px a row; 0 is fine: a hidden bar keeps its laid-out height) and
        // whether it shows; the clamp only guards against nonsense
        const r = a && typeof a === 'object' ? a : {};
        const next = { height: Math.max(0, Math.min(200, Math.round(Number(r.height)) || 0)), shown: r.shown !== false };
        const key = dock ? 'dock' : 'win';
        const was = this.barReport[key];
        if (next.height !== was.height || next.shown !== was.shown) {
          this.barReport[key] = next;
          this.layout();
        }
        return;
      }
      case 'pin':
        // {mode, pinned}: the surface the bar shows (docked, full, floating)
        return a && this.setBarPin(String(a.mode || ''), a.pinned === true);
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
        if (wc) {
          this.endDialogs(e.view_id);
          wc.loadURL(url).catch(() => {});
        } else {
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
          this.endDialogs(e.view_id);
          wc.stop();
          this.report('user_action', 'stop', e, g);
        }
        return;
      case 'new_tab':
        return this.newTab();
      case 'go_to_tab':
        return this.goToTab();
      case 'placement':
        return this.placementAction(String(a || ''));
      case 'send_page':
        return this.sendPage((a && a.note) || '');
      case 'restart_update':
        return this.restartForUpdate();
      case 'permission':
        return a && this.answerPrompt(String(a.id), a.allow === true);
      default:
    }
  }

  // The bar's expand / restore / close (docked): herdr's client that owns the placement
  // changes its dock or Browser view (browser.placement_action; the bar shows the buttons only
  // when herdr advertises placement_actions).
  async placementAction(action) {
    const p = this.dockPlacement;
    const lease = this.link && this.link.lease;
    if (!p || !lease || !['promote', 'restore', 'close'].includes(action)) return;
    try {
      await this.link.request('browser.placement_action', { lease_id: lease.lease_id, id: p.id, action });
    } catch (e) {
      this.say('warn', `herdr did not take it: ${e.message}`);
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
      bar: this.barState(dock),
      status: line,
      update: this.update ? { version: this.update, restarting: this.restarting } : null,
    };
  }

  // Tests (strip_preview): the floating bar's state as `dock` / `kind` / `features` would make it.
  previewState({ dock = false, kind = 'side', features = [], clientTakes = true } = {}) {
    const saved = { features: this.features, placement: this.dockPlacement };
    this.features = new Set(features);
    this.dockPlacement = dock ? { id: 'pl-preview', kind: kind === 'full' ? 'full' : 'side', actions: clientTakes } : null;
    try {
      return { ...this.stripState(), bar: this.barState(dock) };
    } finally {
      this.features = saved.features;
      this.dockPlacement = saved.placement;
    }
  }

  // The bar's surface and its window actions (dock: the placement it covers).
  barState(dock) {
    const placement = dock ? this.dockPlacement : null;
    const kind = placement ? placement.kind : 'side';
    const mode = bar.surface(dock, kind);
    return {
      mode,
      pinned: bar.pinnedFor(this.barPins, mode),
      pulse: this.barPulse,
      menu: this.barMenuOpen,
      actions: bar.windowActions({ docked: dock, kind, features: this.features || null, placementId: placement && placement.id, clientTakes: !!(placement && placement.actions) }),
    };
  }

  // The bar's count pill: pages in the shown session, pages and sessions in all of herdr.
  sessionInfo() {
    return sessionCounts(this.store, this.shownTab);
  }

  // A right-click into a page: its context menu (src/context-menu.js) at the pointer, in the
  // window the page is in (the dock's while docked). Every item acts on the page that was
  // clicked (even if another one is shown by then); back, forward and reload are reported to
  // herdr like the strip's buttons.
  openPageMenu(viewId, params = {}) {
    if (!this.views.has(viewId) || HEADLESS) return 0;
    const wc = this.views.get(viewId).view.webContents;
    const actions = pageMenuActions({
      wc,
      params,
      openTab: (url) => this.onWindowOpen(viewId, url, 'foreground-tab'),
      copyText: (text) => clipboard.writeText(text),
      report: (action) => {
        const hit = this.store.findView(viewId);
        if (hit) this.report('user_action', action, hit.entry, hit.group);
      },
    });
    const template = pageMenuItems(params, this.views.navState(viewId)).map((it) =>
      it.type === 'separator'
        ? it
        : {
            label: it.label,
            enabled: it.enabled,
            click: () => {
              try {
                actions[it.id]();
              } catch (e) {
                this.say('warn', `${it.label} failed: ${e.message}`);
              }
            },
          },
    );
    const win = this.stripWindow();
    if (!win) return 0;
    const menu = Menu.buildFromTemplate(template);
    // the test hook's view of the last one (page_menu)
    this.pageMenu = { view_id: viewId, items: template.map((it) => (it.type === 'separator' ? '-' : it.label)), menu, open: true };
    const shown = this.pageMenu;
    menu.popup({ window: win, callback: () => (shown.open = false) });
    return template.length;
  }

  // The herdr space of `tabId` as a menu sublabel (none when unknown).
  workspaceSublabel(tabId) {
    const ws = (this.store.tabs.get(tabId) || {}).workspace_id;
    return ws ? { sublabel: ws } : {};
  }

  // The pill's menu: the shown herdr tab and Go to herdr tab, then one item per herdr tab
  // with pages; choosing one focuses it in herdr (the companion follows, docked or not).
  openSessionsMenu(dock) {
    const items = sessionMenuItems(this.store, this.shownTab);
    const here = this.shownTab
      ? [
          { label: `This tab · ${this.store.label(this.shownTab)}`, enabled: false },
          { label: 'Go to herdr tab', ...this.workspaceSublabel(this.shownTab), click: () => this.goToTab() },
          { type: 'separator' },
        ]
      : [];
    const template = items.length
      ? [
          ...here,
          { label: 'herdr tabs with pages', enabled: false },
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
      : [...here, { label: 'No herdr tab has pages yet', enabled: false }];
    // the test hook's view of the last one (sessions_menu)
    this.sessionsMenu = template.map((it) => (it.type === 'separator' ? '-' : it.label));
    const win = dock ? this.dockWindow() : this.win;
    if (!win || HEADLESS) return template.length;
    // the bar stays shown while its menu is open
    this.barMenuOpen = true;
    this.pushStrip();
    Menu.buildFromTemplate(template).popup({
      window: win,
      callback: () => {
        this.barMenuOpen = false;
        this.pushStrip();
      },
    });
    return template.length;
  }

  pushStrip() {
    if (this.win && !this.win.isDestroyed()) this.win.webContents.send('strip:state', this.stripPreview ? this.previewState(this.stripPreview) : this.stripState());
    const dock = this.dockBarView();
    if (dock) dock.webContents.send('strip:state', this.stripState({ dock: true }));
  }

  say(level, text, ms = 6000) {
    this.flash = { level, text, until: Date.now() + ms };
    this.pushStrip();
    setTimeout(() => this.pushStrip(), ms + 50).unref();
  }

  // ---------- page API (window.herdr.send) ----------

  // A page's preload asked (invoke): {op: 'hello'} -> {token} | {}, {op: 'send', token, text,
  // data_json} -> {ok, outcome}. Identity from the event only (src/page-api.js); the page hears
  // ok/outcome, never pane or agent names; the strip says who got it.
  async onPageMessage(e, req) {
    const refused = { ok: false, outcome: 'refused' };
    const viewId = this.views.viewIdOf(e.sender);
    const rec = viewId ? this.views.get(viewId) : null;
    const hit = viewId ? this.store.findView(viewId) : null;
    let frame = null;
    let mainFrame = null;
    try {
      frame = e.senderFrame;
      mainFrame = e.sender.mainFrame;
    } catch {}
    const shown = !!(hit && hit.group.tab_id === this.shownTab && hit.group.active === viewId);
    const ctx = {
      features: rec && rec.pageApi ? this.features : null, // a view created without the bridge never sends
      viewId,
      hit,
      frame,
      mainFrame,
      shown,
      token: req && req.token,
      expected: rec && rec.pageToken,
      realpath: (p) => fs.realpathSync(p),
      fileURLToPath,
    };
    const op = req && req.op;
    if (op === 'hello') {
      const ok = pageApi.checkSender(ctx, { hello: true });
      if (!ok.ok) {
        log('page message: hello refused', ok.reason, viewId || '-');
        return {};
      }
      rec.pageToken = crypto.randomBytes(16).toString('hex');
      return { token: rec.pageToken };
    }
    if (op !== 'send') return refused;
    const who = pageApi.checkSender(ctx);
    if (!who.ok) {
      log('page message: refused', who.reason, viewId || '-');
      return who.reason === 'stale_document' ? { ...refused, reason: 'stale_document' } : refused;
    }
    const payload = pageApi.checkPayload(req);
    if (!payload.ok) {
      log('page message: refused', payload.reason, viewId);
      return refused;
    }
    let rate = this.pageRates.get(viewId);
    if (!rate) this.pageRates.set(viewId, (rate = new pageApi.RateLimit()));
    if (!rate.take(Date.now())) {
      log('page message: refused rate', viewId);
      return refused;
    }
    const { group: g, entry: en } = hit;
    const params = { tab_id: g.tab_id, view_id: en.view_id, file: en.file, text: payload.text };
    if (en.id) params.entry = en.id;
    if (payload.data_json !== null) params.data_json = payload.data_json;
    try {
      const res = await this.link.call('browser.page_message', params);
      const name = res.to_name || res.to_pane || 'the agent';
      log('page message', res.outcome, viewId, res.message_id || '');
      if (res.outcome === 'sent') this.say('ok', `Page message sent to ${name}`);
      else this.say('ok', `Page message queued for ${name}${res.reason ? ` (${res.reason})` : ''}`);
      return { ok: true, outcome: res.outcome === 'sent' ? 'sent' : 'queued' };
    } catch (err) {
      log('page message: herdr refused', err.code || '', err.message);
      // the agent clicking its own page hears nothing; the user hears why theirs did not go
      if (err.code !== 'agent_driving') {
        const why =
          err.code === 'agent_suspended'
            ? 'the agent that showed this page is suspended'
            : err.code === 'owner_gone'
              ? 'the agent that showed this page is gone'
              : err.code === 'no_message_reader'
                ? 'the agent that showed this page cannot take page messages'
                : err.code === 'page_messages_off'
                  ? 'page messages are off in herdr'
                  : err.message || err.code || 'refused';
        this.say('warn', `Page message not sent: ${why}`);
      }
      return refused;
    }
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
    this.dialogs.dismissAll(); // no page stays blocked on a dialog while the app quits
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
