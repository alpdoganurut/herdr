// Spike S1-S3 Electron side. Never shows a window, never takes focus.
// Protocol with driver.mjs: stdout lines "SPIKE {json}", stdin lines "{json}" commands.
'use strict';
const { app, BrowserWindow, WebContentsView } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const readline = require('node:readline');

const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'herdr-companion-spike-'));
app.setPath('userData', userData);
app.commandLine.appendSwitch('user-data-dir', userData);
app.commandLine.appendSwitch('remote-debugging-port', '0');
app.commandLine.appendSwitch('remote-debugging-address', '127.0.0.1');
if (process.platform === 'darwin' && app.dock) app.dock.hide();

const say = (obj) => process.stdout.write('SPIKE ' + JSON.stringify(obj) + '\n');

let win;
let parking;
const views = new Map();

function page(color, label) {
  const html = `<!doctype html><html><body style="margin:0;background:${color}">
<h1 id="h">${label}</h1><button id="b" onclick="window.clicks=(window.clicks||0)+1;this.textContent='clicked '+window.clicks" style="width:200px;height:80px">click</button>
<input id="i"></body></html>`;
  return 'data:text/html;base64,' + Buffer.from(html).toString('base64');
}

async function targetIdOf(wc) {
  const dbg = wc.debugger;
  let attached = false;
  try {
    if (!dbg.isAttached()) { dbg.attach('1.3'); attached = true; }
    const info = await dbg.sendCommand('Target.getTargetInfo');
    return info.targetInfo.targetId;
  } finally {
    if (attached) dbg.detach();
  }
}

async function addView(id, color, { attach = true } = {}) {
  const v = new WebContentsView({ webPreferences: { backgroundThrottling: false, sandbox: true, contextIsolation: true } });
  views.set(id, v);
  if (attach) win.contentView.addChildView(v);
  v.setBounds({ x: 0, y: 0, width: 800, height: 600 });
  await v.webContents.loadURL('about:blank#herdr-view=' + id);
  const tid = await targetIdOf(v.webContents);
  await v.webContents.loadURL(page(color, id));
  return tid;
}

async function handle(cmd) {
  try {
    switch (cmd.op) {
      case 'add': {
        const tid = await addView(cmd.id, cmd.color, { attach: cmd.attach !== false });
        return say({ re: cmd.n, ok: true, target_id: tid });
      }
      case 'hide': views.get(cmd.id).setVisible(false); return say({ re: cmd.n, ok: true });
      case 'show': views.get(cmd.id).setVisible(true); return say({ re: cmd.n, ok: true });
      case 'detach': win.contentView.removeChildView(views.get(cmd.id)); return say({ re: cmd.n, ok: true });
      case 'park': {
        if (!parking) parking = new BrowserWindow({ show: false, width: 800, height: 600, paintWhenInitiallyHidden: true });
        const v = views.get(cmd.id);
        parking.contentView.addChildView(v);
        v.setVisible(true);
        v.setBounds({ x: 0, y: 0, width: 800, height: 600 });
        return say({ re: cmd.n, ok: true });
      }
      case 'state': return say({ re: cmd.n, ok: true, visible: win.isVisible(), focused: win.isFocused() });
      case 'quit': say({ re: cmd.n, ok: true }); app.quit(); return;
      default: return say({ re: cmd.n, ok: false, error: 'unknown op' });
    }
  } catch (e) {
    return say({ re: cmd.n, ok: false, error: String(e && e.stack || e) });
  }
}

app.whenReady().then(async () => {
  win = new BrowserWindow({ show: false, width: 800, height: 600, paintWhenInitiallyHidden: true });
  if (!process.env.SPIKE_EMPTY_WINDOW) await win.loadURL('about:blank'); // an unloaded window webContents never answers CDP Page.enable
  const portFile = path.join(userData, 'DevToolsActivePort');
  for (let i = 0; i < 100 && !fs.existsSync(portFile); i++) await new Promise((r) => setTimeout(r, 50));
  let port = 0;
  if (fs.existsSync(portFile)) port = Number(fs.readFileSync(portFile, 'utf8').split('\n')[0]);
  else {
    const { execFileSync } = require('node:child_process');
    const out = execFileSync('lsof', ['-a', '-p', String(process.pid), '-iTCP', '-sTCP:LISTEN', '-Fn', '-P', '-n'], { encoding: 'utf8' });
    const m = out.match(/n127\.0\.0\.1:(\d+)/); port = m ? Number(m[1]) : 0;
    say({ log: 'no DevToolsActivePort; lsof port ' + port });
  }
  say({ ready: true, port, electron: process.versions.electron, chrome: process.versions.chrome, pid: process.pid });
  readline.createInterface({ input: process.stdin }).on('line', (l) => { if (l.trim()) handle(JSON.parse(l)); });
});
app.on('window-all-closed', () => {});
app.on('will-quit', () => { try { fs.rmSync(userData, { recursive: true, force: true }); } catch {} });
