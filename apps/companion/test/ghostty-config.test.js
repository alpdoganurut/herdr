'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { readGhosttyConfig, parseText, applyArgs, defaults, configPaths } = require('../src/dock/ghostty-config.js');

test('the geometry keys are read; everything else is ignored', () => {
  const cfg = defaults();
  parseText(cfg, '# comment\nfont-size = 13\nwindow-padding-x = 10\nwindow-padding-y = "6,8"\nwindow-padding-balance = true\nmacos-titlebar-style = native\n');
  assert.deepEqual(cfg.paddingX, [10, 10]);
  assert.deepEqual(cfg.paddingY, [6, 8]);
  assert.equal(cfg.balance, true);
  assert.equal(cfg.titlebarStyle, 'native');
});

test('malformed values keep the default; later lines win', () => {
  const cfg = defaults();
  parseText(cfg, 'window-padding-x = wide\nwindow-padding-y = 1,2,3\nmacos-titlebar-style = fancy\nwindow-padding-x = 4\nwindow-padding-x = 5\n');
  assert.deepEqual(cfg.paddingX, [5, 5]);
  assert.deepEqual(cfg.paddingY, [2, 2]);
  assert.equal(cfg.titlebarStyle, 'transparent');
});

test('config files in order, one level of config-file (optional ones may be missing), flags win', () => {
  const home = '/home/u';
  const files = {
    [path.join(home, '.config', 'ghostty', 'config')]: 'window-padding-x = 4\nconfig-file = extra\nconfig-file = ?missing\n',
    [path.join(home, '.config', 'ghostty', 'extra')]: 'window-padding-y = 7\nconfig-file = deeper\n',
    [path.join(home, '.config', 'ghostty', 'deeper')]: 'window-padding-x = 99\n',
    [path.join(home, 'Library', 'Application Support', 'com.mitchellh.ghostty', 'config')]: 'window-padding-balance = true\n',
  };
  const readFile = (p) => {
    if (!(p in files)) throw new Error('ENOENT');
    return files[p];
  };
  const cfg = readGhosttyConfig({ env: {}, home, readFile, argv: ['--window-padding-y=3,4', '--quit-after-last-window-closed=true'] });
  assert.deepEqual(cfg.paddingX, [4, 4]); // the second-level include is not read
  assert.deepEqual(cfg.paddingY, [3, 4]); // the flag wins
  assert.equal(cfg.balance, true);
  assert.equal(cfg.sources.length, 3);
});

test('XDG_CONFIG_HOME moves the first config file', () => {
  assert.equal(configPaths({ XDG_CONFIG_HOME: '/x' }, '/h')[0], path.join('/x', 'ghostty', 'config'));
  assert.deepEqual(applyArgs(defaults(), ['--window-padding-balance']).balance, true);
});

test('an empty value resets the key to its default, as in Ghostty; a bare key or flag means true', () => {
  const cfg = defaults();
  parseText(cfg, 'window-padding-x = 10\nwindow-padding-y = 6,8\nwindow-padding-balance = true\nmacos-titlebar-style = hidden\n');
  parseText(cfg, 'window-padding-x =\nwindow-padding-y = \nwindow-padding-balance =\nmacos-titlebar-style = ""\n');
  assert.deepEqual(cfg.paddingX, [2, 2]);
  assert.deepEqual(cfg.paddingY, [2, 2]);
  assert.equal(cfg.balance, false);
  assert.equal(cfg.titlebarStyle, 'transparent');
  // a key alone on its line is the flag `--key`: true for a boolean, nothing for the others
  const bare = defaults();
  parseText(bare, 'window-padding-x = 6\nwindow-padding-balance\nwindow-padding-x\n');
  assert.equal(bare.balance, true);
  assert.deepEqual(bare.paddingX, [6, 6]);
  // flags: `--key` is true, `--key=` resets
  const flags = applyArgs(defaults(), ['--window-padding-balance', '--window-padding-x=9']);
  assert.equal(flags.balance, true);
  assert.deepEqual(flags.paddingX, [9, 9]);
  applyArgs(flags, ['--window-padding-balance=', '--window-padding-x=']);
  assert.equal(flags.balance, false);
  assert.deepEqual(flags.paddingX, [2, 2]);
});

test("Ghostty's booleans and whole-point paddings; anything else keeps what was set", () => {
  const cfg = defaults();
  parseText(cfg, 'window-padding-balance = t\n');
  assert.equal(cfg.balance, true);
  parseText(cfg, 'window-padding-balance = 0\n');
  assert.equal(cfg.balance, false);
  parseText(cfg, 'window-padding-balance = TRUE\nwindow-padding-x = 4\nwindow-padding-x = 2.5\nwindow-padding-x = 3,\nmacos-titlebar-style = Hidden\n');
  assert.equal(cfg.balance, false);
  assert.deepEqual(cfg.paddingX, [4, 4]);
  assert.equal(cfg.titlebarStyle, 'transparent');
});
