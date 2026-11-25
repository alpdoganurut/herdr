// The app test/e2e-remote-client.mjs starts: the companion under a hard timer, never shown or
// focused, with a look into its main process over the child's IPC channel. Nothing here can
// open a window, reach the clipboard, start another app or grant a permission.
'use strict';
const { app, session, webContents, net, BrowserWindow } = require('electron');
const path = require('node:path');

if (app.dock) app.dock.hide();
try {
  app.setActivationPolicy('prohibited');
} catch {}
setTimeout(() => app.quit(), 48000);
setTimeout(() => app.exit(99), 56000);
process.on('uncaughtException', (e) => console.log('[uncaught]', e && e.stack));
app.commandLine.appendSwitch('use-mock-keychain');
app.commandLine.appendSwitch('mute-audio');
app.commandLine.appendSwitch('use-fake-device-for-media-stream');
if (process.env.E2E_SPKI) app.commandLine.appendSwitch('ignore-certificate-errors-spki-list', process.env.E2E_SPKI);

app.on('browser-window-created', (_e, w) => w.on('show', () => console.log('[SHOWN]', w.getTitle())));

// what the companion's permission handlers decide is recorded, and always answered with no
const permissions = [];
app.on('session-created', (ses) => {
  ses.on('will-download', (e) => e.preventDefault());
  const request = ses.setPermissionRequestHandler.bind(ses);
  ses.setPermissionRequestHandler = (h) =>
    request(
      h &&
        ((wc, p, cb, d) => {
          const rec = { permission: p, verdict: 'pending' };
          permissions.push(rec);
          h(
            wc,
            p,
            (v) => {
              rec.verdict = v;
              cb(false);
            },
            d,
          );
        }),
    );
});
let loginAsked = 0;
app.on('login', () => loginAsked++);
let certsOffered = 0;
app.on('select-client-certificate', () => certsOffered++);

const SES = { default: () => session.defaultSession, web: () => session.fromPartition('persist:herdr-companion'), ui: () => session.fromPartition('herdr-ui') };
const ses = (n) => (SES[n] ? SES[n]() : session.fromPartition(n));
const outcome = (p) => Promise.race([p, new Promise((r) => setTimeout(() => r('timeout'), 4000))]);
const ops = {
  switches: () => Object.fromEntries(['disable-quic', 'host-resolver-rules', 'proxy-server', 'proxy-bypass-list'].map((k) => [k, app.commandLine.hasSwitch(k) ? app.commandLine.getSwitchValue(k) : null])),
  contents: () =>
    webContents.getAllWebContents().map((wc) => ({
      id: wc.id,
      url: wc.getURL(),
      type: wc.getType(),
      webrtc: wc.getWebRTCIPHandlingPolicy(),
      partition: Object.keys(SES).find((k) => wc.session === SES[k]()) || 'other',
      file: wc.session.protocol.isProtocolHandled('file'),
    })),
  session: async (name) => ({
    file: ses(name).protocol.isProtocolHandled('file'),
    proxy: await ses(name).resolveProxy('http://127.0.0.1:1/'),
    proxyName: await ses(name).resolveProxy('https://a.test/'),
  }),
  // the ids DevTools stays open on (`ever`: opened at all), each in a host window never shown
  devtools: async (ids, ever) => {
    const all = ids.map((id) => {
      const wc = webContents.fromId(id);
      const host = new BrowserWindow({ show: false, focusable: false, webPreferences: { sandbox: true } });
      const t = { id, wc, host, opened: false };
      wc.once('devtools-opened', () => (t.opened = true));
      wc.setDevToolsWebContents(host.webContents);
      wc.openDevTools({ mode: 'detach', activate: false });
      return t;
    });
    for (let i = 0; i < 50 && !all.every((t) => t.opened); i++) await new Promise((r) => setTimeout(r, 100));
    await new Promise((r) => setTimeout(r, 500));
    return all.filter((t) => {
      const open = t.wc.isDevToolsOpened();
      if (open) t.wc.closeDevTools();
      t.host.destroy();
      return open || (ever && t.opened);
    }).map((t) => t.id);
  },
  fetchFile: (name, url) => outcome(ses(name).fetch(url).then((r) => `LOADED ${r.status}`, (e) => e.message)),
  // the first request of a fresh session, at once (the race a cold session has)
  cold: (name, url) => outcome(ses(name).fetch(url).then((r) => `status ${r.status}`, (e) => e.message)),
  // a cold session's own gate, with the app's code
  gate: async (name, url) => {
    const remote = require('../src/remote.js');
    const s = ses(name);
    const open = await remote.warm(s, 4000);
    return { open, then: await outcome(s.fetch(url).then((r) => `status ${r.status}`, (e) => e.message)) };
  },
  load: (id, url) => {
    const wc = webContents.fromId(id);
    return outcome(wc.loadURL(url).then(() => 'LOADED', (e) => e.message)).then((r) => [r, wc.getURL()]);
  },
  permissions: () => permissions,
  asked: () => ({ loginAsked, certsOffered }),
  cert: () =>
    new Promise((resolve) => {
      let prevented = false;
      const n = app.listenerCount('select-client-certificate');
      let answered = null;
      app.emit('select-client-certificate', { preventDefault: () => (prevented = true) }, null, 'https://a.test/', [{ subjectName: 'fake' }], (...a) => (answered = a));
      resolve({ listeners: n, prevented, answered });
    }),
  netDefault: (url) => outcome(net.fetch(url).then((r) => `LOADED ${r.status}`, (e) => e.message)),
};
process.on('message', async ({ id, op, args }) => {
  let out;
  try {
    out = { id, value: await ops[op](...(args || [])) };
  } catch (e) {
    out = { id, error: e && e.stack };
  }
  try {
    process.send(out);
  } catch {}
});

require(path.join(__dirname, '..', 'src', 'main.js'));
