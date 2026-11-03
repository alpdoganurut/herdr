// The Ghostty settings the dock's geometry depends on (pure parser + a reader).
//
// Read from the user's config files (later files and later lines win, one level of
// `config-file`, a leading `?` marks an optional one) and the Ghostty process's own
// command line (`--window-padding-x=10`, which wins over the files):
//   window-padding-x / window-padding-y   "2" or "2,4" (points; left,right / top,bottom)
//   window-padding-balance                true | false
//   macos-titlebar-style                  transparent | native | hidden | tabs
// Everything else is ignored. Unknown or malformed values keep the default.

'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const KEYS = new Set(['window-padding-x', 'window-padding-y', 'window-padding-balance', 'macos-titlebar-style']);

function defaults() {
  return { paddingX: [2, 2], paddingY: [2, 2], balance: false, titlebarStyle: 'transparent', sources: [] };
}

function unquote(v) {
  const t = v.trim();
  if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) return t.slice(1, -1);
  return t;
}

function parsePair(value) {
  const parts = unquote(value).split(',').map((p) => p.trim());
  const nums = parts.map(Number);
  if (!parts.length || parts.length > 2 || nums.some((n) => !Number.isFinite(n) || n < 0)) return null;
  return nums.length === 1 ? [nums[0], nums[0]] : nums;
}

// Apply one `key = value` to `cfg` (in place). Returns whether the key was one of ours.
function applySetting(cfg, key, value) {
  if (!KEYS.has(key)) return false;
  switch (key) {
    case 'window-padding-x': {
      const pair = parsePair(value);
      if (pair) cfg.paddingX = pair;
      break;
    }
    case 'window-padding-y': {
      const pair = parsePair(value);
      if (pair) cfg.paddingY = pair;
      break;
    }
    case 'window-padding-balance': {
      const v = unquote(value).toLowerCase();
      if (v === 'true' || v === '') cfg.balance = true;
      else if (v === 'false') cfg.balance = false;
      break;
    }
    case 'macos-titlebar-style': {
      const v = unquote(value).toLowerCase();
      if (['transparent', 'native', 'hidden', 'tabs'].includes(v)) cfg.titlebarStyle = v;
      break;
    }
    default:
  }
  return true;
}

// Parse a config file's text into `cfg`; returns the `config-file` includes it names.
function parseText(cfg, text) {
  const includes = [];
  for (const raw of String(text).split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const eq = line.indexOf('=');
    if (eq < 0) continue;
    const key = line.slice(0, eq).trim();
    const value = line.slice(eq + 1);
    if (key === 'config-file') {
      const v = unquote(value);
      if (v) includes.push(v);
      continue;
    }
    applySetting(cfg, key, value);
  }
  return includes;
}

// `--window-padding-x=10` style flags from a Ghostty command line (they win over files).
function applyArgs(cfg, argv) {
  for (const arg of argv || []) {
    if (!arg.startsWith('--')) continue;
    const eq = arg.indexOf('=');
    const key = eq < 0 ? arg.slice(2) : arg.slice(2, eq);
    const value = eq < 0 ? 'true' : arg.slice(eq + 1);
    applySetting(cfg, key, value);
  }
  return cfg;
}

// The config files Ghostty reads on macOS, in order (later wins).
function configPaths(env = process.env, home = os.homedir()) {
  const xdg = env.XDG_CONFIG_HOME || path.join(home, '.config');
  const app = path.join(home, 'Library', 'Application Support', 'com.mitchellh.ghostty');
  return [
    path.join(xdg, 'ghostty', 'config'),
    path.join(xdg, 'ghostty', 'config.ghostty'),
    path.join(app, 'config'),
    path.join(app, 'config.ghostty'),
  ];
}

// Read the user's settings (and the process's flags, when given). `readFile` is for tests.
function readGhosttyConfig({ env = process.env, home = os.homedir(), argv = null, readFile = (p) => fs.readFileSync(p, 'utf8') } = {}) {
  const cfg = defaults();
  const load = (file, depth) => {
    let text;
    try {
      text = readFile(file);
    } catch {
      return;
    }
    cfg.sources.push(file);
    const includes = parseText(cfg, text);
    if (depth > 0) return; // one level of config-file
    for (let inc of includes) {
      const optional = inc.startsWith('?');
      if (optional) inc = inc.slice(1);
      if (inc.startsWith('~/')) inc = path.join(home, inc.slice(2));
      if (!path.isAbsolute(inc)) inc = path.join(path.dirname(file), inc);
      load(inc, depth + 1);
    }
  };
  for (const file of configPaths(env, home)) load(file, 0);
  if (argv) applyArgs(cfg, argv);
  return cfg;
}

module.exports = { readGhosttyConfig, parseText, applyArgs, applySetting, configPaths, defaults };
