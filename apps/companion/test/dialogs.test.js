'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { DialogBook, normalizeRequest, replyFor } = require('../src/dialogs.js');

function book() {
  let changes = 0;
  const b = new DialogBook({ onChange: () => changes++ });
  return { b, changes: () => changes };
}
const recorder = () => {
  const got = [];
  const fn = (v) => got.push(v);
  fn.got = got;
  return fn;
};

test('requests are normalized: known types only, strings only, a default only for prompt', () => {
  assert.deepEqual(normalizeRequest({ type: 'prompt', message: 'm', default_value: 'd' }), { type: 'prompt', message: 'm', default_value: 'd' });
  assert.deepEqual(normalizeRequest({ type: 'confirm', message: 'm', default_value: 'd' }), { type: 'confirm', message: 'm', default_value: '' });
  assert.deepEqual(normalizeRequest({ type: 'alert', message: 3 }), { type: 'alert', message: '', default_value: '' });
  assert.equal(normalizeRequest({ type: 'print' }), null);
  assert.equal(normalizeRequest(null), null);
  assert.equal(normalizeRequest({ type: 'prompt', message: 'x'.repeat(200000) }).message.length, 100000);
});

test('the page gets what a browser returns: alert nothing, confirm a boolean, prompt text or null', () => {
  const p = { type: 'prompt', default_value: 'd', value: 'd' };
  assert.equal(replyFor({ type: 'alert' }, true), null);
  assert.equal(replyFor({ type: 'confirm' }, true), true);
  assert.equal(replyFor({ type: 'confirm' }, false), false);
  assert.equal(replyFor(p, true, 'x'), 'x');
  assert.equal(replyFor(p, true, ''), '');
  assert.equal(replyFor(p, true), 'd');
  assert.equal(replyFor({ ...p, value: 'typed' }, true), 'typed');
  assert.equal(replyFor(p, false, 'x'), null);
});

test('a prompt opens, shows, takes typing and answers its page once', () => {
  const { b, changes } = book();
  const reply = recorder();
  const d = b.open('v1', { type: 'prompt', message: 'Copy this:', default_value: 'abc' }, reply, { frame: '7', info: { origin: 'https://a.test' } });
  assert.equal(b.top('v1'), d);
  assert.equal(d.value, 'abc');
  assert.equal(d.origin, 'https://a.test');
  assert.equal(b.viewOf(d.id), 'v1');
  assert.equal(changes(), 1);
  assert.equal(b.setValue('v1', d.id, 'abcd'), true);
  assert.equal(b.setValue('v1', 'other', 'zzz'), false);
  assert.equal(b.answer('v1', { id: 'other', accept: true }), null, 'a stale id answers nothing');
  assert.equal(b.answer('v1', { id: d.id, accept: true }), d);
  assert.deepEqual(reply.got, ['abcd']);
  assert.equal(b.top('v1'), null);
  assert.equal(b.answer('v1', { accept: true }), null);
  assert.deepEqual(reply.got, ['abcd'], 'answered once');
  assert.equal(changes(), 2);
});

test('one dialog per view: the next waits behind it (a cross-origin frame asking meanwhile)', () => {
  const { b } = book();
  const r1 = recorder();
  const r2 = recorder();
  const d1 = b.open('v1', { type: 'confirm', message: 'one' }, r1, { frame: 'main' });
  const d2 = b.open('v1', { type: 'prompt', message: 'two', default_value: 'z' }, r2, { frame: 'sub' });
  assert.equal(b.top('v1'), d1);
  assert.deepEqual(b.list().map((x) => [x.id, x.queued]), [[d1.id, 1]]);
  b.answer('v1', { accept: true });
  assert.deepEqual(r1.got, [true]);
  assert.equal(b.top('v1'), d2);
  b.answer('v1', { accept: false });
  assert.deepEqual(r2.got, [null]);
});

test('views are independent; list gives herdr the shown dialog of each with its target', () => {
  const { b } = book();
  b.open('v1', { type: 'alert', message: 'a' }, recorder());
  b.open('v2', { type: 'prompt', message: 'p', default_value: 'd' }, recorder());
  const list = b.list((v) => `T-${v}`);
  assert.deepEqual(
    list.map(({ view_id, target_id, type, message, default_value }) => ({ view_id, target_id, type, message, default_value })),
    [
      { view_id: 'v1', target_id: 'T-v1', type: 'alert', message: 'a', default_value: '' },
      { view_id: 'v2', target_id: 'T-v2', type: 'prompt', message: 'p', default_value: 'd' },
    ],
  );
  assert.equal(b.has('v1'), true);
  assert.equal(b.has('v3'), false);
});

test('a navigation cancels the dialogs of the frame that leaves (the main frame: all of them)', () => {
  const { b } = book();
  const main = recorder();
  const sub = recorder();
  const conf = recorder();
  b.open('v1', { type: 'prompt', message: 'm' }, main, { frame: 'f1' });
  b.open('v1', { type: 'prompt', message: 's' }, sub, { frame: 'f2' });
  b.open('v1', { type: 'confirm', message: 'c' }, conf, { frame: 'f2' });
  assert.equal(b.dismissView('v1', { frame: 'f2' }), 2);
  assert.deepEqual(sub.got, [null]);
  assert.deepEqual(conf.got, [false]);
  assert.equal(b.top('v1').message, 'm');
  assert.equal(b.dismissView('v1', { frame: 'f9' }), 0);
  assert.equal(b.dismissView('v1'), 1);
  assert.deepEqual(main.got, [null]);
  assert.equal(b.dismissView('v1'), 0);
});

test('a quit answers every page; a bad request or no view is answered at once', () => {
  const { b, changes } = book();
  const r1 = recorder();
  const r2 = recorder();
  b.open('v1', { type: 'alert', message: 'a' }, r1);
  b.open('v2', { type: 'confirm', message: 'c' }, r2);
  assert.equal(b.dismissAll(), 2);
  assert.deepEqual([r1.got, r2.got], [[null], [false]]);
  assert.equal(b.list().length, 0);
  const before = changes();
  const r3 = recorder();
  assert.equal(b.open('v1', { type: 'nope' }, r3), null);
  assert.deepEqual(r3.got, [null]);
  const r4 = recorder();
  assert.equal(b.open(null, { type: 'confirm' }, r4), null);
  assert.deepEqual(r4.got, [false]);
  assert.equal(changes(), before, 'nothing opened');
});

test('a reply that throws (a disposed frame) does not break the book', () => {
  const { b } = book();
  b.open('v1', { type: 'alert' }, () => {
    throw new Error('Render frame was disposed');
  });
  assert.doesNotThrow(() => b.answer('v1', { accept: true }));
  assert.equal(b.top('v1'), null);
});

test('the seq moves on every change (herdr reconciles by it)', () => {
  const { b } = book();
  const s0 = b.seq;
  b.open('v1', { type: 'alert' }, recorder());
  const s1 = b.seq;
  b.answer('v1', { accept: true });
  assert.ok(s1 > s0 && b.seq > s1);
});
