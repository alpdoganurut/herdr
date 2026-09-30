// herdr browser: live smoke of the sidecar against a running herdr-launched Chromium.
// Not part of nextest. Usage (from the installed host dir):
//   node smoke.mjs <devtools-port> [url]
// Drives host.mjs over a pipe exactly like the server does and prints each reply.
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const [port, url = 'https://example.com/'] = process.argv.slice(2);
if (!port) { console.error('usage: node smoke.mjs <devtools-port> [url]'); process.exit(2); }

const host = spawn(process.execPath, [path.join(here, 'host.mjs')], { stdio: ['pipe', 'pipe', 'inherit'] });
const rl = readline.createInterface({ input: host.stdout });
const pending = new Map();
let id = 0;
rl.on('line', (line) => {
  let msg; try { msg = JSON.parse(line); } catch { return console.log('noise:', line); }
  if (msg.event) return console.log('event', JSON.stringify(msg));
  const p = pending.get(msg.id); pending.delete(msg.id);
  if (p) p(msg);
});
const call = (op, extra = {}) => new Promise((resolve) => {
  const req = { id: ++id, op, deadline_ms: 30000, ...extra };
  pending.set(req.id, resolve);
  host.stdin.write(JSON.stringify(req) + '\n');
});
const show = (label, reply) => console.log(label, JSON.stringify(reply).slice(0, 400));

const hello = await call('hello'); show('hello', hello);
const attached = await call('attach', { profile: 'smoke', args: { port: Number(port) } }); show('attach', attached);
const opened = await call('open', { profile: 'smoke', args: { url, background: true } }); show('open', opened);
const target = opened.result && opened.result.target;
if (target) {
  show('read', await call('read', { profile: 'smoke', target, args: { format: 'markdown' } }));
  show('snapshot', await call('read', { profile: 'smoke', target, args: { format: 'snapshot' } }));
  show('links', await call('links', { profile: 'smoke', target, args: {} }));
  const shot = path.join(os.tmpdir(), `herdr-smoke-${Date.now()}.jpg`);
  show('screenshot', await call('screenshot', { profile: 'smoke', target, args: { path: shot, inline_path: shot.replace('.jpg', '.inline.jpg'), format: 'jpeg', quality: 70, max_px: 800 } }));
  show('console', await call('console', { profile: 'smoke', target, args: {} }));
  show('tabs', await call('tabs', { profile: 'smoke' }));
  show('close', await call('close', { profile: 'smoke', target }));
}
host.stdin.end();
