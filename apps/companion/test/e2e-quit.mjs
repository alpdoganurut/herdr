// Quit a headless companion this checkout's e2e started, through its control page (its
// DevTools port from <profile>/DevToolsActivePort): `node test/e2e-quit.mjs <profile dir>`.
// The cleanup path of the e2e scripts; never a signal.
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');
const dir = process.argv[2];
let port;
try {
  port = Number(fs.readFileSync(path.join(dir, 'DevToolsActivePort'), 'utf8').split('\n')[0]);
} catch {
  process.exit(0); // never started
}
try {
  const b = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 5000 });
  const page = b.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith('herdr-companion://control/'));
  if (page) await page.evaluate(() => globalThis.__herdrCompanion.quit()).catch(() => {});
  await b.close().catch(() => {});
} catch {
  // gone already
}
