'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { fileURLToPath } = require('node:url');
const { checkSender, checkPayload, RateLimit, optedIn, TEXT_MAX, DATA_MAX } = require('../src/page-api.js');

const FILE = '/r/board.html';
const entry = (over = {}) => ({ kind: 'html_view', file: FILE, page_api: true, allow_network: false, opened_by: 'agent:w1:p2', view_id: 'v1', ...over });
const main = { url: `file://${FILE}`, parent: null, isDestroyed: () => false, detached: false };
const ctx = (over = {}) => ({
  features: new Set(['page_messages']),
  viewId: 'v1',
  hit: { group: { tab_id: 'w1:t1' }, entry: entry() },
  frame: main,
  mainFrame: main,
  shown: true,
  token: 'k1',
  expected: 'k1',
  // a fake file system: /r/link.html is a symlink to /r/board.html
  realpath: (p) => (p === '/r/link.html' ? FILE : p),
  fileURLToPath,
  ...over,
});
const reason = (c, o) => checkSender(c, o).reason;

test('the top frame of an opted-in, shown view of the exact file may send', () => {
  assert.deepStrictEqual(checkSender(ctx()), { ok: true });
  assert.deepStrictEqual(checkSender(ctx({ frame: { ...main, url: `file://${FILE}#step-3` }, mainFrame: null })), { ok: true }, 'a hash is the same document');
  assert.deepStrictEqual(checkSender(ctx({ frame: { ...main, url: 'file:///r/link.html' }, mainFrame: null })), { ok: true }, 'the same real file');
});

test('everything else is refused, decided from the event, never from the page', () => {
  assert.strictEqual(reason(ctx({ features: new Set() })), 'off', 'herdr turned page messages off');
  assert.strictEqual(reason(ctx({ viewId: null, hit: null })), 'unknown_view', 'not a page view (the strip, the control page)');
  for (const over of [{ page_api: false }, { allow_network: true }, { kind: 'web' }, { opened_by: 'user' }, { opened_by: 'agent:' }, { file: null }]) {
    assert.strictEqual(reason(ctx({ hit: { group: {}, entry: entry(over) } })), 'not_opted_in', JSON.stringify(over));
  }
  assert.strictEqual(reason(ctx({ frame: null })), 'no_frame', 'its document went (Electron: senderFrame null)');
  assert.strictEqual(reason(ctx({ frame: { ...main, isDestroyed: () => true } })), 'frame_gone');
  assert.strictEqual(reason(ctx({ frame: { ...main, detached: true } })), 'frame_gone');
  const iframe = { url: `file://${FILE}`, parent: main, isDestroyed: () => false };
  assert.strictEqual(reason(ctx({ frame: iframe })), 'subframe', 'an iframe, even of the same file');
  assert.strictEqual(reason(ctx({ frame: { ...main }, mainFrame: main })), 'subframe', 'not the main frame object');
  const away = { ...main, url: 'file:///r/other.html' };
  assert.strictEqual(reason(ctx({ frame: away, mainFrame: away })), 'wrong_file', 'navigated away inside the directory');
  const web = { ...main, url: 'https://evil.test/' };
  assert.strictEqual(reason(ctx({ frame: web, mainFrame: web })), 'wrong_file');
  assert.strictEqual(reason(ctx({ token: 'old' })), 'stale_document');
  assert.strictEqual(reason(ctx({ expected: null, token: null })), 'stale_document');
  assert.strictEqual(reason(ctx({ shown: false })), 'not_shown', 'a hidden view is what agents drive');
  // a hello needs the identity, not a token or the shown gate
  assert.deepStrictEqual(checkSender(ctx({ token: null, expected: null, shown: false }), { hello: true }), { ok: true });
  assert.strictEqual(reason(ctx({ frame: iframe }), { hello: true }), 'subframe');
});

test('the payload is capped and JSON only', () => {
  assert.deepStrictEqual(checkPayload({ text: 'hi', data_json: '{"a":1}' }), { ok: true, text: 'hi', data_json: '{"a":1}' });
  assert.deepStrictEqual(checkPayload({ data_json: '[1]' }), { ok: true, text: '', data_json: '[1]' });
  assert.strictEqual(checkPayload({ text: 'x'.repeat(TEXT_MAX) }).ok, true);
  assert.strictEqual(checkPayload({ text: 'x'.repeat(TEXT_MAX + 1) }).reason, 'too_large');
  assert.strictEqual(checkPayload({ text: 'a', data_json: JSON.stringify('y'.repeat(DATA_MAX)) }).reason, 'too_large');
  assert.strictEqual(checkPayload({ text: 'a', data_json: '{nope' }).reason, 'invalid');
  assert.strictEqual(checkPayload({ text: 5 }).reason, 'invalid');
  assert.strictEqual(checkPayload({ text: ' ' }).reason, 'empty');
  assert.strictEqual(checkPayload(null).reason, 'invalid');
});

test('a flood is cut: a burst of 3, then one per 2 s, 30 an hour', () => {
  const t0 = 1_000_000;
  const rate = new RateLimit(undefined, t0);
  const sent = Array.from({ length: 10 }, () => rate.take(t0)).filter(Boolean).length;
  assert.strictEqual(sent, 3);
  assert.strictEqual(rate.take(t0 + 1000), false);
  assert.strictEqual(rate.take(t0 + 2000), true);
  let n = 4;
  for (let t = t0 + 4000; t < t0 + 3600 * 1000; t += 2000) if (rate.take(t)) n++;
  assert.strictEqual(n, 30, 'the hourly cap');
});

test('optedIn mirrors herdr: html views without network shown by an agent', () => {
  assert.ok(optedIn(entry()));
  assert.ok(!optedIn(entry({ allow_network: true })));
  assert.ok(!optedIn(null));
});
