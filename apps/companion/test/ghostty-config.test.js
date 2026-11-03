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
