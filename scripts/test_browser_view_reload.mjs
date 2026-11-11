// Shown pages keep the reader's place across a reload (fork): the sidecar
// (host.mjs over its JSON-lines pipe, attached over CDP to a headless shell
// this script starts) shows a local HTML file with `show` (watch on), the
// reader scrolls, types, opens <details>, focuses a field and selects text,
// the file is rewritten (content added above the reader's place, a late image
// above it), and after the watch's reload the same paragraph is at the top
// and the inner scroll area, the fields, the open <details>, the focus and
// the selection are back. Gone elements are skipped; a page that scrolls
// itself after load, or restores its own scroll, is not overridden; a side
// file written next to the page does not reload it.
// Run: node scripts/test_browser_view_reload.mjs   (also run by
// integration::browser_assets::tests when node is on PATH; skipped with exit
// 0 when playwright-core or a browser binary is not found)
//   HERDR_TEST_PLAYWRIGHT_CORE=<dir of playwright-core>  (default: apps/companion's)
//   HERDR_TEST_CHROMIUM=<browser binary>  (default: playwright's, else its newest headless shell)
// Never touches herdr's own browser, profiles or state.
import { existsSync, mkdtempSync, mkdirSync, cpSync, symlinkSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import http from 'node:http';
import os from 'node:os';
import readline from 'node:readline';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..');
const assets = join(repo, 'src/integration/assets/browser');
function check(cond, msg) { if (!cond) throw new Error(msg); }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const require = createRequire(import.meta.url);
const pwCandidates = [process.env.HERDR_TEST_PLAYWRIGHT_CORE, join(repo, 'apps/companion/node_modules/playwright-core'), join(repo, 'apps/companion/install/node_modules/playwright-core')].filter(Boolean);
const pwDir = pwCandidates.find((dir) => existsSync(join(dir, 'package.json')));
if (!pwDir) { console.log('skip: no playwright-core (set HERDR_TEST_PLAYWRIGHT_CORE)'); process.exit(0); }
const { chromium } = require(pwDir);
function browserBinary() {
  if (process.env.HERDR_TEST_CHROMIUM) return existsSync(process.env.HERDR_TEST_CHROMIUM) ? process.env.HERDR_TEST_CHROMIUM : null;
  try { const p = chromium.executablePath(); if (p && existsSync(p)) return p; } catch {}
  const cache = process.platform === 'darwin' ? join(os.homedir(), 'Library/Caches/ms-playwright') : join(os.homedir(), '.cache/ms-playwright');
  if (!existsSync(cache)) return null;
  const shells = readdirSync(cache).filter((d) => d.startsWith('chromium_headless_shell-')).sort((a, b) => Number(b.split('-')[1]) - Number(a.split('-')[1]));
  for (const d of shells) {
    for (const sub of ['chrome-mac', 'chrome-mac-arm64', 'chrome-linux', 'chrome-headless-shell-mac-arm64', 'chrome-headless-shell-linux64']) {
      for (const bin of ['headless_shell', 'chrome-headless-shell']) {
        const p = join(cache, d, sub, bin);
        if (existsSync(p)) return p;
      }
    }
  }
  return null;
}
const binary = browserBinary();
if (!binary) { console.log('skip: no Chromium binary (set HERDR_TEST_CHROMIUM)'); process.exit(0); }

const tmp = mkdtempSync(join(os.tmpdir(), 'herdr-view-reload-'));
const hostDir = join(tmp, 'host');
const views = join(tmp, 'views');
mkdirSync(join(hostDir, 'node_modules'), { recursive: true });
mkdirSync(views);
for (const f of ['host.mjs', 'activity.mjs', 'extract.mjs', 'package.json']) cpSync(join(assets, f), join(hostDir, f));
cpSync(join(assets, 'companion'), join(hostDir, 'companion'), { recursive: true });
symlinkSync(pwDir, join(hostDir, 'node_modules/playwright-core'), 'dir');

// an image that answers late (a page's picture still being fetched after DOMContentLoaded)
const server = http.createServer((req, res) => {
  setTimeout(() => { res.setHeader('content-type', 'image/svg+xml'); res.end('<svg xmlns="http://www.w3.org/2000/svg" width="200" height="900"><rect width="200" height="900" fill="#ccc"/></svg>'); }, 700);
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const slowImg = (n) => `http://127.0.0.1:${server.address().port}/slow.svg?${n}`;

// The page: paragraphs p0..p59 (100 px each), an inner scroll area without an id, fields,
// two <details>. `top` is added above the paragraphs; `title` tells the versions apart.
const PARAS = Array.from({ length: 60 }, (_, i) => `<p id="p${i}">paragraph ${i} ${'words '.repeat(8)}</p>`).join('\n');
const page = ({ title, top = '', box = true, extraHead = '', script = '' }) => `<!doctype html>
<html><head><meta charset="utf-8"><title>${title}</title>
<style>body{margin:0;font:14px sans-serif} p{height:100px;margin:0} .box{height:150px;overflow:auto;border:1px solid #888} .box div{height:1200px}</style>${extraHead}</head>
<body>
${top}
<form><input id="name" value=""><textarea name="notes"></textarea>
<select id="pick"><option value="a">A</option><option value="b">B</option><option value="c">C</option></select>
<label><input type="checkbox" name="agree" value="yes">agree</label>
<label><input type="radio" name="size" value="s" checked>s</label><label><input type="radio" name="size" value="m">m</label>
<input type="password" id="secret"></form>
${box ? '<section><div class="box"><div>inner scroll area</div></div></section>' : ''}
<details id="more"><summary>more</summary>details body</details>
${box ? '<details><summary>other</summary>other body</details>' : ''}
${PARAS}
${script}
</body></html>
`;

const chrome = spawn(binary, ['--headless', '--remote-debugging-port=0', `--user-data-dir=${join(tmp, 'profile')}`, '--no-first-run', '--no-default-browser-check', '--window-size=800,600', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
let host = null;
const cleanup = () => {
  try { if (host) host.stdin.end(); } catch {}
  try { chrome.kill('SIGKILL'); } catch {}
  server.close();
  try { rmSync(tmp, { recursive: true, force: true }); } catch {}
};
let failed = false;
try {
  const port = await new Promise((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(() => reject(new Error(`no DevTools port: ${buf.slice(-400)}`)), 15000);
    chrome.stderr.on('data', (chunk) => {
      buf += chunk;
      const m = buf.match(/DevTools listening on ws:\/\/[^:]+:(\d+)\//);
      if (m) { clearTimeout(timer); resolve(Number(m[1])); }
    });
    chrome.on('exit', (code) => reject(new Error(`the browser exited (${code}): ${buf.slice(-400)}`)));
  });

  let stderr = '';
  host = spawn(process.execPath, [join(hostDir, 'host.mjs')], { cwd: hostDir, stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, NODE_NO_WARNINGS: '1' } });
  host.stderr.on('data', (chunk) => { stderr += chunk; });
  const pending = new Map();
  const logs = [];
  readline.createInterface({ input: host.stdout }).on('line', (line) => {
    let msg; try { msg = JSON.parse(line); } catch { return; }
    if (msg.event === 'log') logs.push(msg.text);
    if (msg.event) return;
    const done = pending.get(msg.id); pending.delete(msg.id);
    if (done) done(msg);
  });
  let id = 0;
  const call = (op, extra = {}) => new Promise((resolve) => {
    const req = { id: ++id, op, deadline_ms: 15000, ...extra };
    pending.set(req.id, resolve);
    host.stdin.write(JSON.stringify(req) + '\n');
  });
  check((await call('hello')).ok, 'hello');
  const attached = await call('attach', { profile: 't', args: { port }, deadline_ms: 20000 });
  check(attached.ok, `attach: ${JSON.stringify(attached.error)}`);

  const show = async (file, extra = {}) => {
    const shown = await call('show', { profile: 't', args: { file, focus: true, ...extra } });
    check(shown.ok && shown.result.watching, `show: ${JSON.stringify(shown.error || shown.result)}`);
    return shown.result.target;
  };
  const evalIn = async (target, expr) => {
    const r = await call('eval', { profile: 't', target, args: { expr } });
    check(r.ok, `eval ${expr.slice(0, 60)}: ${JSON.stringify(r.error)}`);
    return r.result.value;
  };
  // the watch's reload: the new title, then the restore's settle (the late image, 1.5 s at most)
  const reloaded = async (target, title, ms = 8000) => {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const r = await call('eval', { profile: 't', target, args: { expr: 'document.title' } });
      if (r.ok && r.result.value === title) { await sleep(1800); return true; }
      await sleep(100);
    }
    return false;
  };
  const topOf = (target, pid) => evalIn(target, `Math.round(document.getElementById('${pid}').getBoundingClientRect().top)`);

  // --- 1. content added above, a late image above the reader: everything comes back
  const file = join(views, 'report.html');
  writeFileSync(file, page({ title: 'v1' }));
  const t1 = await show(file);
  await sleep(300);
  await evalIn(t1, `(() => {
    document.getElementById('p20').scrollIntoView();
    scrollBy(0, 30);
    document.querySelector('.box').scrollTop = 300;
    document.getElementById('name').value = 'Ada';
    document.querySelector('[name=notes]').value = 'line one\\nline two';
    document.getElementById('pick').value = 'c';
    document.querySelector('[name=agree]').checked = true;
    document.querySelector('[name=size][value=m]').checked = true;
    document.getElementById('secret').value = 'hunter2';
    document.getElementById('more').open = true;
    const name = document.getElementById('name');
    name.focus({ preventScroll: true });
    name.setSelectionRange(1, 3);
    return true;
  })()`);
  const before = await topOf(t1, 'p20');
  // the agent rewrites the page: a header and a late image above everything
  writeFileSync(file, page({ title: 'v2', top: `<h1 style="height:400px;margin:0">New header</h1><img src="${slowImg(1)}" alt="chart">`, extraHead: '<style>html{overflow-anchor:none}</style>' }));
  // (without allow_network the image request is blocked: scenario 2 has the late load)
  check(await reloaded(t1, 'v2'), `the rewritten page reloads (${logs.slice(-3).join(' | ')})`);
  const after = await evalIn(t1, `({
    p20: Math.round(document.getElementById('p20').getBoundingClientRect().top),
    box: document.querySelector('.box').scrollTop,
    name: document.getElementById('name').value,
    notes: document.querySelector('[name=notes]').value,
    pick: document.getElementById('pick').value,
    agree: document.querySelector('[name=agree]').checked,
    size: document.querySelector('[name=size]:checked').value,
    secret: document.getElementById('secret').value,
    more: document.getElementById('more').open,
    focus: document.activeElement && document.activeElement.id,
    sel: [document.activeElement.selectionStart, document.activeElement.selectionEnd],
    restoration: history.scrollRestoration,
  })`);
  check(Math.abs(after.p20 - before) <= 2, `the same paragraph stays at the top (${before} -> ${after.p20})`);
  check(after.box === 300, `the inner scroll area keeps its place (${after.box})`);
  check(after.name === 'Ada' && after.notes === 'line one\nline two' && after.pick === 'c' && after.agree === true && after.size === 'm', `the fields keep their input (${JSON.stringify(after)})`);
  check(after.secret === '', 'a password field is never carried over');
  check(after.more === true, 'the open <details> stays open');
  check(after.focus === 'name' && after.sel[0] === 1 && after.sel[1] === 3, `the focused field and its selection come back (${JSON.stringify(after)})`);
  check(after.restoration === 'auto', `the page's scroll restoration is back to auto (${after.restoration})`);
  console.log('ok: a rewritten page keeps scroll, inner scroll, fields, details and focus');

  // --- 2. a late image above the reader on a page that shows network images
  const fileImg = join(views, 'late.html');
  writeFileSync(fileImg, page({ title: 'i1', extraHead: '<style>html{overflow-anchor:none}</style>' }));
  const t2 = await show(fileImg, { allow_network: true });
  await sleep(300);
  await evalIn(t2, `document.getElementById('p30').scrollIntoView(), true`);
  const beforeImg = await topOf(t2, 'p30');
  writeFileSync(fileImg, page({ title: 'i2', top: `<img src="${slowImg(2)}" alt="late">`, extraHead: '<style>html{overflow-anchor:none}</style>' }));
  check(await reloaded(t2, 'i2'), 'the page with the late image reloads');
  const imgH = await evalIn(t2, `document.querySelector('img').naturalHeight`);
  const afterImg = await topOf(t2, 'p30');
  check(imgH === 900 && Math.abs(afterImg - beforeImg) <= 2, `the paragraph stays at the top after the late image loaded (${beforeImg} -> ${afterImg}, image ${imgH}; ${logs.filter((l) => l.includes('view ')).slice(-3).join(' | ')})`);
  console.log('ok: the scroll anchor survives a late image above it');

  // --- 3. the selection; gone elements are skipped
  writeFileSync(file, page({ title: 'v3' }));
  check(await reloaded(t1, 'v3'), 'v3 reloads');
  await evalIn(t1, `(() => {
    document.activeElement.blur();
    document.querySelector('.box').scrollTop = 200;
    document.querySelectorAll('details')[1].open = true;
    const a = document.getElementById('p25').firstChild;
    const b = document.getElementById('p26').firstChild;
    getSelection().setBaseAndExtent(a, 3, b, 9);
    document.getElementById('p25').scrollIntoView();
    return getSelection().toString();
  })()`);
  const selBefore = await evalIn(t1, 'getSelection().toString()');
  writeFileSync(file, page({ title: 'v4', box: false }));
  check(await reloaded(t1, 'v4'), 'v4 reloads');
  const v4 = await evalIn(t1, `({ sel: getSelection().toString(), box: !!document.querySelector('.box'), p25: Math.round(document.getElementById('p25').getBoundingClientRect().top), more: document.getElementById('more').open })`);
  check(v4.sel === selBefore && selBefore.startsWith('agraph 25'), `the selection comes back (${JSON.stringify(selBefore)} -> ${JSON.stringify(v4.sel)})`);
  check(!v4.box && Math.abs(v4.p25) <= 2 && v4.more === true, `a gone scroll area and details are skipped, the rest restored (${JSON.stringify(v4)})`);
  check(logs.some((l) => /reloaded, .* [1-9]\d* gone/.test(l)), `the reload logs what was gone (${logs.filter((l) => l.includes('reloaded')).slice(-1)})`);
  console.log('ok: the selection comes back; gone elements are skipped');

  // --- 4. a page that scrolls itself after load, or restores its own scroll, is not overridden
  const self = `<script>addEventListener('load', () => document.getElementById('p3').scrollIntoView());</script>`;
  writeFileSync(file, page({ title: 'v5', script: self }));
  check(await reloaded(t1, 'v5'), 'v5 reloads');
  await evalIn(t1, `document.getElementById('p40').scrollIntoView(), true`);
  writeFileSync(file, page({ title: 'v6', script: self }));
  check(await reloaded(t1, 'v6'), 'v6 reloads');
  const p3 = await topOf(t1, 'p3');
  check(Math.abs(p3) <= 2, `a page that scrolls itself on load keeps its own scroll (p3 at ${p3})`);
  // the same page with a late image (allow_network): its load comes after the restore (no CSS
  // scroll anchoring: Chromium's own adjustment for the image would undo the page's scroll)
  const fileSelf = join(views, 'self.html');
  const noAnchoring = '<style>html{overflow-anchor:none}</style>';
  writeFileSync(fileSelf, page({ title: 's1', script: self, extraHead: noAnchoring }));
  const t3 = await show(fileSelf, { allow_network: true });
  await sleep(300);
  await evalIn(t3, `document.getElementById('p40').scrollIntoView(), true`);
  writeFileSync(fileSelf, page({ title: 's2', script: self, extraHead: noAnchoring, top: `<img src="${slowImg(3)}">` }));
  check(await reloaded(t3, 's2'), 's2 reloads');
  const p3late = await topOf(t3, 'p3');
  const lateLog = logs.filter((l) => l.includes('reloaded')).slice(-1)[0] || '';
  check(Math.abs(p3late) <= 2 && lateLog.includes('(page '), `a page that scrolls itself on a late load keeps its own scroll (p3 at ${p3late}; ${lateLog})`);
  await call('close', { profile: 't', target: t3 });
  const own = `<script>history.scrollRestoration = 'manual'; addEventListener('DOMContentLoaded', () => document.getElementById('p5').scrollIntoView());</script>`;
  writeFileSync(file, page({ title: 'v7', script: own }));
  check(await reloaded(t1, 'v7'), 'v7 reloads');
  await evalIn(t1, `document.getElementById('p45').scrollIntoView(), true`);
  writeFileSync(file, page({ title: 'v8', script: own }));
  check(await reloaded(t1, 'v8'), 'v8 reloads');
  const p5 = await topOf(t1, 'p5');
  check(Math.abs(p5) <= 2 && await evalIn(t1, 'history.scrollRestoration') === 'manual', `a page that restores its own scroll is left to it (p5 at ${p5})`);
  console.log('ok: a page that scrolls itself is not overridden');

  // --- 5. a side file next to the page does not reload it
  await evalIn(t1, 'window.__mark = 1');
  writeFileSync(join(views, 'chart.png'), 'not really a png');
  await sleep(1200);
  check(await evalIn(t1, 'window.__mark') === 1, 'a side file written next to the page does not reload it');
  console.log('ok: a side file does not reload the page');
  await call('close', { profile: 't', target: t1 });
  await call('close', { profile: 't', target: t2 });
} catch (err) {
  failed = true;
  console.error(`FAIL: ${err && err.stack || err}`);
} finally {
  cleanup();
}
if (failed) process.exit(1);
console.log('ok: shown pages keep the reader\'s place across reloads');
process.exit(0);
