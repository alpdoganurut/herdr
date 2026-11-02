// Screenshots of the companion bar (strip.html) in fixed states, light and dark, from a HIDDEN window.
//
//   npx electron test/ui-shots.cjs <out-dir>
//
// Loads the real strip page through the same herdr-companion: scheme and preload as the app, feeds it
// synthetic strip states over the preload's channel, and captures the chrome with webContents.capturePage.
// The window is never shown (no dock icon); the run exits by itself.
'use strict';
const { app, BrowserWindow, ipcMain, protocol, session, nativeTheme, net } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { pathToFileURL } = require('node:url');

const OUT = path.resolve(process.argv[process.argv.length - 1]);
const UI_DIR = path.join(__dirname, '..', 'src', 'ui');
const SCHEME = 'herdr-companion';
const WIDTH = 760;

app.setPath('userData', fs.mkdtempSync(path.join(os.tmpdir(), 'hcomp-shots-')));
if (process.platform === 'darwin' && app.dock) app.dock.hide();
protocol.registerSchemesAsPrivileged([{ scheme: SCHEME, privileges: { standard: true, secure: true, supportFetchAPI: true } }]);

const URL = 'https://docs.example.com/api/v2/authentication#rate-limits';
const base = () => ({
  inset: process.platform === 'darwin',
  session: { tab_id: 'w1:t1', label: 'api-docs', workspace: 'w1' },
  tabs: [
    { view_id: 'v1', title: 'Docs', url: URL, kind: 'page', active: true, needs_you: false, opened_by: 'user', loading: false },
    { view_id: 'v2', title: 'Issue 42', url: 'https://github.com/x/y/issues/42', kind: 'page', active: false, needs_you: false, opened_by: 'claude', loading: false },
  ],
  active: { view_id: 'v1', url: URL, title: 'Docs', kind: 'page' },
  nav: { canGoBack: true, canGoForward: false, loading: false },
  prompt: null,
  attached: true,
  status: null,
});

const STATES = {
  normal: (s) => s,
  attention: (s) => {
    s.tabs[1].needs_you = true;
    s.tabs.push({ view_id: 'v3', title: 'Authentication & rate limits, a very long page title that has to truncate', url: URL, kind: 'page', active: false, needs_you: false, opened_by: 'user', loading: false });
    return s;
  },
  loading: (s) => {
    s.tabs[0].loading = true;
    s.nav.loading = true;
    return s;
  },
  note: (s) => s,
  permission: (s) => {
    s.prompt = { id: '1', origin: 'docs.example.com', label: 'use your camera', permission: 'media' };
    return s;
  },
  status: (s) => {
    s.status = { level: 'ok', text: 'Sent to claude (w1:p2) with a note and 214 chars of selection' };
    return s;
  },
};

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

app.whenReady().then(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  const ui = session.fromPartition('herdr-ui-shots');
  ui.protocol.handle(SCHEME, (req) => {
    const file = path.join(UI_DIR, decodeURIComponent(new globalThis.URL(req.url).pathname));
    return net.fetch(pathToFileURL(file).toString());
  });
  ipcMain.handle('herdr-companion:strip', () => null);
  const win = new BrowserWindow({
    show: false,
    width: WIDTH,
    height: 220,
    paintWhenInitiallyHidden: true,
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
    webPreferences: { session: ui, preload: path.join(__dirname, '..', 'src', 'preload', 'strip.js'), sandbox: true, contextIsolation: true, backgroundThrottling: false },
  });
  await win.loadURL(`${SCHEME}://ui/strip.html`);
  const written = [];
  for (const scheme of ['light', 'dark']) {
    nativeTheme.themeSource = scheme;
    for (const [name, make] of Object.entries(STATES)) {
      await win.webContents.executeJavaScript(`document.getElementById('sendbar').hidden = true; document.activeElement && document.activeElement.blur(); 0`);
      win.webContents.send('strip:state', make(base()));
      await wait(150);
      if (name === 'note') await win.webContents.executeJavaScript(`document.getElementById('send').click(); 0`);
      await wait(250);
      const h = await win.webContents.executeJavaScript(`Math.ceil(document.getElementById('chrome').getBoundingClientRect().height)`);
      const img = await win.webContents.capturePage({ x: 0, y: 0, width: WIDTH, height: h });
      const file = path.join(OUT, `bar-${scheme}-${name}.png`);
      fs.writeFileSync(file, img.toPNG());
      written.push(`${file} ${img.getSize().width}x${img.getSize().height} chrome=${h}px`);
    }
  }
  console.log(written.join('\n'));
  win.destroy();
  app.quit();
});
