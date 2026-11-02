// Spike driver: spawns spike/main.js under Electron, connects playwright-core over CDP,
// runs S1-S3 and prints a JSON report. Quits Electron through its own stdin control path.
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { inflateSync } from 'node:zlib';
import readline from 'node:readline';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const electronBin = require('electron');
const { chromium } = require('playwright-core');
const here = path.dirname(fileURLToPath(import.meta.url));

const child = spawn(electronBin, [path.join(here, 'main.js')], { stdio: ['pipe', 'pipe', 'pipe'] });
child.stderr.on('data', (d) => { if (process.env.SPIKE_VERBOSE) process.stderr.write(d); });
const pending = new Map();
let readyResolve;
const ready = new Promise((r) => (readyResolve = r));
readline.createInterface({ input: child.stdout }).on('line', (line) => {
  if (!line.startsWith('SPIKE ')) return;
  const msg = JSON.parse(line.slice(6));
  if (msg.log) { report.logs = (report.logs || []).concat(msg.log); return; }
  if (msg.ready) return readyResolve(msg);
  const p = pending.get(msg.re);
  if (p) { pending.delete(msg.re); p(msg); }
});
let n = 0;
const cmd = (op, args = {}) => new Promise((resolve) => {
  const id = ++n;
  pending.set(id, resolve);
  child.stdin.write(JSON.stringify({ n: id, op, ...args }) + '\n');
});

// Minimal PNG decode: returns the RGBA of the pixel at (x, y) and a "blank" verdict.
function pngStats(buf) {
  let off = 8; let w = 0; let h = 0; let ct = 0; const idat = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off); const type = buf.toString('ascii', off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (type === 'IHDR') { w = data.readUInt32BE(0); h = data.readUInt32BE(4); ct = data[9]; }
    if (type === 'IDAT') idat.push(data);
    off += 12 + len;
  }
  const bpp = ct === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(idat));
  const stride = w * bpp; const out = Buffer.alloc(stride * h);
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)]; const src = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? out[y * stride + x - bpp] : 0; const b = y > 0 ? out[(y - 1) * stride + x] : 0;
      const c = x >= bpp && y > 0 ? out[(y - 1) * stride + x - bpp] : 0;
      let v = src[x];
      if (f === 1) v += a; else if (f === 2) v += b; else if (f === 3) v += (a + b) >> 1;
      else if (f === 4) { const p = a + b - c; const pa = Math.abs(p - a); const pb = Math.abs(p - b); const pc = Math.abs(p - c); v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c; }
      out[y * stride + x] = v & 255;
    }
  }
  const px = (x, y) => Array.from(out.subarray(y * stride + x * bpp, y * stride + x * bpp + 3));
  return { w, h, corner: px(w - 5, h - 5) };
}

const withTimeout = (p, ms, what) => Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new Error(`timeout ${ms}ms: ${what}`)), ms))]);
const report = { results: {} };
const rec = (k, v) => { report.results[k] = v; };

async function shotCheck(page, expect, label) {
  const t0 = Date.now();
  try {
    const png = await withTimeout(page.screenshot({ timeout: 4000 }), 5000, 'screenshot');
    const s = pngStats(png);
    // Screenshots come back colour-managed (sRGB red 255,0,0 reads ~234,51,35), so compare the dominant channel.
    const dom = (c) => c.map((v) => (v > 128 ? 1 : 0)).join('');
    const ok = dom(s.corner) === dom(expect) || (expect[1] === 160 && s.corner[1] > 128 && s.corner[0] < 128);
    rec(label + '.screenshot', { ok, ms: Date.now() - t0, size: [s.w, s.h], corner: s.corner, expect });
  } catch (e) { rec(label + '.screenshot', { ok: false, ms: Date.now() - t0, error: e.message }); }
  try {
    await withTimeout(page.click('#b', { timeout: 3000 }), 4000, 'click');
    const clicks = await page.evaluate(() => window.clicks || 0);
    rec(label + '.click', { ok: clicks > 0, clicks });
  } catch (e) { rec(label + '.click', { ok: false, error: e.message }); }
  try {
    await withTimeout(page.fill('#i', 'hello'), 4000, 'fill');
    rec(label + '.fill', { ok: (await page.inputValue('#i')) === 'hello' });
  } catch (e) { rec(label + '.fill', { ok: false, error: e.message }); }
  try {
    const raf = await withTimeout(page.evaluate(() => new Promise((r) => requestAnimationFrame(() => r(true)))), 2000, 'rAF');
    rec(label + '.raf', { ok: raf });
  } catch (e) { rec(label + '.raf', { ok: false, error: e.message }); }
  try {
    const vis = await page.evaluate(() => ({ state: document.visibilityState, hidden: document.hidden }));
    rec(label + '.visibility', vis);
  } catch (e) { rec(label + '.visibility', { error: e.message }); }
}

async function main() {
  const info = await withTimeout(ready, 30000, 'electron ready');
  report.electron = info.electron; report.chrome = info.chrome; report.pid = info.pid; report.port = info.port;
  const t1 = await cmd('add', { id: 'v1', color: 'rgb(255,0,0)' });
  const t2 = await cmd('add', { id: 'v2', color: 'rgb(0,0,255)' });
  const t3 = await cmd('add', { id: 'v3', color: 'rgb(0,160,0)' });
  rec('S2.debugger_target_ids', { ok: [t1, t2, t3].every((t) => t.ok && t.target_id), ids: [t1.target_id, t2.target_id, t3.target_id], errors: [t1, t2, t3].filter((t) => !t.ok).map((t) => t.error) });

  // S1: connectOverCDP without --remote-allow-origins
  let browser;
  try {
    browser = await withTimeout(chromium.connectOverCDP(`http://127.0.0.1:${info.port}`), 15000, 'connectOverCDP');
    rec('S1.connect', { ok: true, version: browser.version(), contexts: browser.contexts().length });
  } catch (e) {
    rec('S1.connect', { ok: false, error: e.message });
    console.log(JSON.stringify(report, null, 2));
    await cmd('quit');
    return;
  }
  const ctx = browser.contexts()[0];
  const pages = ctx.pages();
  rec('S1.pages', { ok: pages.length >= 3, urls: pages.map((p) => p.url().slice(0, 60)) });

  // S2: map page -> targetId via CDP session
  const map = {};
  for (const p of pages) {
    try {
      const s = await ctx.newCDPSession(p);
      const ti = await s.send('Target.getTargetInfo');
      map[ti.targetInfo.targetId] = p;
      await s.detach();
    } catch (e) { rec('S2.cdp_session_error', e.message); }
  }
  const p1 = map[t1.target_id]; const p2 = map[t2.target_id]; const p3 = map[t3.target_id];
  rec('S2.match', { ok: !!(p1 && p2 && p3), matched: [!!p1, !!p2, !!p3] });
  if (!(p1 && p2 && p3)) { console.log(JSON.stringify(report, null, 2)); await cmd('quit'); return; }

  // S1 basics: goto / evaluate on a page
  try {
    await p3.goto('data:text/html,<title>t3</title><body style="background:rgb(0,160,0)"><button id=b onclick="window.clicks=(window.clicks||0)+1">b</button><input id=i>');
    rec('S1.goto_evaluate', { ok: (await p3.evaluate(() => document.title)) === 't3' });
  } catch (e) { rec('S1.goto_evaluate', { ok: false, error: e.message }); }

  // ctx.on('page') for a view created after connect
  const newPage = new Promise((r) => ctx.once('page', (p) => r(p)));
  const t4 = await cmd('add', { id: 'v4', color: 'rgb(200,200,0)' });
  try {
    const p4 = await withTimeout(newPage, 5000, 'ctx page event');
    rec('S1.page_event', { ok: true, url: p4.url().slice(0, 40), target_id: t4.target_id });
  } catch (e) { rec('S1.page_event', { ok: false, error: e.message }); }

  // S3: window is show:false the whole time (paintWhenInitiallyHidden)
  rec('S3.window_state', await cmd('state'));
  await shotCheck(p1, [255, 0, 0], 'S3.a_visible_view_in_hidden_window');
  await cmd('hide', { id: 'v2' });
  await shotCheck(p2, [0, 0, 255], 'S3.b_setVisible_false');
  await cmd('detach', { id: 'v3' });
  await shotCheck(p3, [0, 160, 0], 'S3.c_detached_view');
  await cmd('park', { id: 'v3' });
  await shotCheck(p3, [0, 160, 0], 'S3.d_parked_in_hidden_window');
  const t5 = await cmd('add', { id: 'v5', color: 'rgb(255,0,255)', attach: false });
  await new Promise((r) => setTimeout(r, 500));
  const p5 = ctx.pages().find((p) => p.url().startsWith('data:'));
  rec('S3.e_never_attached_target', { ok: t5.ok, target_id: t5.target_id, error: t5.error });
  // find p5 by target id
  for (const p of ctx.pages()) {
    const s = await ctx.newCDPSession(p); const ti = await s.send('Target.getTargetInfo'); await s.detach();
    if (ti.targetInfo.targetId === t5.target_id) { await shotCheck(p, [255, 0, 255], 'S3.e_never_attached'); break; }
  }
  void p5;
  rec('S3.window_state_after', await cmd('state'));
  await browser.close().catch(() => {});
  await cmd('quit');
}

main().then(() => {
  console.log(JSON.stringify(report, null, 2));
}).catch(async (e) => {
  console.log(JSON.stringify({ fatal: e.stack, report }, null, 2));
  try { await withTimeout(cmd('quit'), 3000, 'quit'); } catch {}
}).finally(() => {
  setTimeout(() => { if (child.exitCode === null) console.error('electron still running pid', child.pid); process.exit(0); }, 4000);
  child.on('exit', () => process.exit(0));
});
