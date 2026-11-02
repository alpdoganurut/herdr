// herdr+ companion browser: Electron main process (contract §6).
// One window; a WebContentsView per page; one group of pages per herdr tab; follows herdr focus.
'use strict';
const { app, BrowserWindow, WebContentsView, ipcMain, protocol, session, Menu, net } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { pathToFileURL } = require('node:url');
const { GroupStore } = require('./groups.js');
const { ViewManager, within } = require('./views.js');
const { HerdrLink } = require('./host.js');
const { resolveSocketPath } = require('./herdr-api.js');

const VERSION = require('../package.json').version;
const UI_DIR = path.join(__dirname, 'ui');
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
if (profileDir) app.setPath('userData', path.resolve(profileDir));
// Chromium writes DevToolsActivePort only when user-data-dir is a switch (spike S1).
app.commandLine.appendSwitch('user-data-dir', app.getPath('userData'));
app.commandLine.appendSwitch('remote-debugging-port', '0');
app.commandLine.appendSwitch('remote-debugging-address', '127.0.0.1');
if (HEADLESS && process.platform === 'darwin' && app.dock) app.dock.hide();

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
    this.chromeHeight = 76;
    this.shownTab = null;
    this.flash = null; // transient strip message
    this.syncTimer = null;
    this.syncing = false;
    this.lastSyncAt = 0;
    this.reports = [];
    this.tokens = REPORT_RATE;
    this.tokensAt = Date.now();
    this.refreshTimer = null;
    this.focusTimer = null;
    this.selfRaiseUntil = 0; // window focus events before this are our own raise
    this.views = new ViewManager({
      onNav: (id, patch) => this.onNav(id, patch),
      onState: (id) => this.onViewState(id),
      onError: (id, kind, text) => this.onPageError(id, kind, text),
      onOpen: (from, url, disposition, opts) => this.onWindowOpen(from, url, disposition, opts),
      onKey: () => false,
      onPermission: (id, req, done) => this.onPermission(id, req, done),
      onLeave: (id, origin) => this.onLeave(id, origin),
    });
    this.prompts = []; // pending permission prompts {id, view_id, permission, label, origin, dones[], timer}
    this.nextPrompt = 1;
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
      if (!this.win || e.sender !== this.win.webContents) return null;
      return this.stripAction(name, a);
    });

    this.win = new BrowserWindow({
      show: false,
      width: 1200,
      height: 820,
      minWidth: 480,
      minHeight: 300,
      title: 'herdr companion',
      backgroundColor: '#1c1d21',
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
    this.win.on('resize', () => this.layout());
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
      }),
    });
    this.link.on('status', (s) => {
      log('status', s.state, s.text);
      this.pushStrip();
    });
    this.link.on('lease', (lease) => this.onLease(lease));
    this.link.on('lost', () => this.pushStrip());
    this.link.on('event', (ev) => this.onEvent(ev));
    this.link.start();
    setInterval(() => this.flushReports(), REPORT_FLUSH_MS).unref();

    if (!HEADLESS) this.win.show();
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
    this.store.resetLease();
    await this.refreshState();
    if (this.link) this.link.releaseEvents();
    this.scheduleSync(0);
  }

  async refreshState() {
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
    if (focus && (focus !== prevFocus || !this.shownTab)) await this.showGroup(focus, { reason: 'follow', focusMoved: focus !== prevFocus });
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
    if (!this.win) return;
    const [w, h] = this.win.getContentSize();
    const top = this.chromeHeight;
    const e = this.activeEntry();
    this.views.layout(e ? e.view_id : null, { x: 0, y: top, width: w, height: Math.max(0, h - top) });
    const label = this.shownTab ? this.store.label(this.shownTab) : null;
    this.win.setTitle(label ? `herdr · ${label}` : 'herdr companion');
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
        case 'quit':
          if (!HEADLESS) return errOut('refused', 'quit through the control page is for headless runs only');
          setTimeout(() => app.quit(), 20);
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

  async ctlFocus(viewId) {
    const hit = this.store.findView(viewId);
    if (!hit) return errOut('view_not_found', `no view ${viewId}`);
    this.store.select(viewId);
    this.scheduleSync();
    await this.showGroup(hit.group.tab_id, { reason: 'agent' });
    hit.entry.attention = true; // the strip dot; cleared when the user selects it or herdr focus shows it
    this.pushStrip();
    if (!HEADLESS && this.win) {
      this.selfRaiseUntil = Date.now() + 1000;
      this.win.show();
      this.win.focus();
    } else log('focus: window raise suppressed (headless)');
    return { ok: true, view_id: viewId };
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
      case 'send':
        return this.win && this.win.webContents.executeJavaScript("document.getElementById('send').click()", true);
      case 'go':
        return this.goToTab();
      default:
    }
  }

  focusUrl() {
    if (!this.win || HEADLESS) return;
    this.win.webContents.focus();
    this.win.webContents.send('strip:focus-url');
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

  async stripAction(name, a) {
    const g = this.shownGroup();
    const e = this.activeEntry(g);
    const wc = e && this.views.has(e.view_id) ? this.views.get(e.view_id).view.webContents : null;
    switch (name) {
      case 'ready':
        return this.pushStrip();
      case 'chrome-height': {
        const h = Math.max(40, Math.min(200, Number(a) || 76));
        if (h !== this.chromeHeight) {
          this.chromeHeight = h;
          this.layout();
        }
        return;
      }
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
      case 'permission':
        return a && this.answerPrompt(String(a.id), a.allow === true);
      default:
    }
  }

  stripState() {
    const g = this.shownGroup();
    const active = this.activeEntry(g);
    const status = this.link ? this.link.status : { state: 'connecting', text: 'starting…' };
    let line = null;
    if (this.flash && this.flash.until > Date.now()) line = { level: this.flash.level, text: this.flash.text };
    else if (status.state !== 'registered') line = { level: 'warn', text: status.text };
    return {
      inset: process.platform === 'darwin',
      session: this.shownTab ? { tab_id: this.shownTab, label: this.store.label(this.shownTab) } : null,
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
    };
  }

  pushStrip() {
    if (!this.win || this.win.isDestroyed()) return;
    this.win.webContents.send('strip:state', this.stripState());
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
    if (this.tokens < 1) return; // over the rate: dropped here, never an error
    this.tokens -= 1;
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
