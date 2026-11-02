'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { GroupStore } = require('../src/groups.js');
const { LineParser, resolveSocketPath } = require('../src/herdr-api.js');

const hostState = (extra = {}) => ({
  focused_tab_id: 'w1:t1',
  tabs: [
    { tab_id: 'w1:t1', workspace_id: 'w1', label: 'planner' },
    {
      tab_id: 'w1:t2', workspace_id: 'w1', label: 'builder', key: 'g-aaa', rev: 3, active: 'b2',
      entries: [
        { id: 'b1', kind: 'web', url: 'https://a.test/', title: 'A' },
        { id: 'b2', kind: 'html_view', url: 'file:///r/x.html', file: '/r/x.html', allow_network: false, title: 'X' },
      ],
    },
  ],
  ...extra,
});

test('host_state seeds placeholder groups and keeps the restored active entry', () => {
  const s = new GroupStore();
  const out = s.applyHostState(hostState());
  assert.deepEqual(out.created, ['w1:t2']);
  const g = s.group('w1:t2');
  assert.equal(g.key, 'g-aaa');
  assert.equal(g.entries.length, 2);
  assert.ok(g.entries.every((e) => e.placeholder && !e.target));
  assert.equal(g.entries.find((e) => e.view_id === g.active).id, 'b2');
  assert.equal(g.entries[1].root, '/r');
  assert.equal(s.focusedTabId, 'w1:t1');
  assert.equal(s.group('w1:t1'), null, 'tabs without state get no group until used');
});

test('a re-read keeps live groups, follows a key to a new tab id, and drops gone tabs', () => {
  const s = new GroupStore();
  s.applyHostState(hostState());
  s.addEntry('w1:t1', { url: 'https://live.test/' });
  s.group('w1:t1').key = 'g-live';
  const live = s.group('w1:t2');
  const out = s.applyHostState({
    focused_tab_id: 'w2:t9',
    tabs: [
      { tab_id: 'w2:t9', workspace_id: 'w2', label: 'builder', key: 'g-aaa', entries: [{ id: 'b1', url: 'https://stale/' }] },
    ],
  });
  assert.deepEqual(out.moved, [{ from: 'w1:t2', to: 'w2:t9' }]);
  assert.equal(s.group('w2:t9'), live, 'the live group object survives, herdr entries do not overwrite it');
  assert.equal(live.entries.length, 2);
  assert.deepEqual(out.dropped.map((g) => g.key), ['g-live']);
  assert.equal(s.group('w1:t1'), null);
});

test('sync carries dirty groups only, minted keys and entry ids are applied', () => {
  const s = new GroupStore();
  s.applyHostState(hostState());
  s.dirty.clear();
  const e = s.addEntry('w1:t1', { url: 'https://n.test/', target: 'T1' }, { background: false });
  const p = s.takeSync('L1');
  assert.equal(p.seq, 1);
  assert.equal(p.lease_id, 'L1');
  assert.equal(p.groups.length, 1);
  assert.equal(p.groups[0].tab_id, 'w1:t1');
  assert.equal(p.groups[0].key, undefined);
  assert.equal(p.groups[0].active, e.view_id, 'an entry without id is named by its view id');
  assert.deepEqual(p.groups[0].entries[0], { view_id: e.view_id, kind: 'web', url: 'https://n.test/', title: '', target: 'T1', opened_by: 'user' });
  assert.equal(s.takeSync('L1'), null, 'nothing dirty, nothing to send');
  s.applySynced({ keys: { 'w1:t1': 'g-new', [`w1:t1/${e.view_id}`]: 'b7' }, unknown_tabs: [] });
  assert.equal(s.group('w1:t1').key, 'g-new');
  assert.equal(e.id, 'b7');
  s.updateEntry(e.view_id, { title: 'N' });
  const p2 = s.takeSync('L1');
  assert.equal(p2.seq, 2);
  assert.equal(p2.groups[0].key, 'g-new');
  assert.equal(p2.groups[0].active, 'b7');
});

test('unknown_tabs drop their groups; a new lease resets seq and resyncs everything', () => {
  const s = new GroupStore();
  s.applyHostState(hostState());
  s.takeSync('L1');
  const dropped = s.applySynced({ keys: {}, unknown_tabs: ['w1:t2'] });
  assert.equal(dropped.length, 1);
  assert.equal(s.group('w1:t2'), null);
  s.addEntry('w1:t1', { url: 'https://x/' });
  s.takeSync('L1');
  s.resetLease();
  const p = s.takeSync('L2');
  assert.equal(p.seq, 1);
  assert.equal(p.groups.length, 1);
});

test('removing the active entry selects its neighbour; unchanged updates stay clean', () => {
  const s = new GroupStore();
  const a = s.addEntry('t', { url: 'a' });
  const b = s.addEntry('t', { url: 'b' }, { background: false });
  const c = s.addEntry('t', { url: 'c' });
  assert.equal(s.group('t').active, b.view_id);
  s.removeEntry(b.view_id);
  assert.equal(s.group('t').active, c.view_id);
  s.removeEntry(c.view_id);
  assert.equal(s.group('t').active, a.view_id);
  s.dirty.clear();
  s.updateEntry(a.view_id, { url: 'a' });
  assert.equal(s.dirty.size, 0);
});

test('a group refuses its 33rd page', () => {
  const s = new GroupStore();
  for (let i = 0; i < 32; i++) s.addEntry('t', { url: 'u' + i });
  assert.throws(() => s.addEntry('t', { url: 'x' }), { code: 'too_many_views' });
});

test('line parser splits chunks, skips blank and bad lines', () => {
  const got = [];
  const bad = [];
  const p = new LineParser((v) => got.push(v), (l) => bad.push(l));
  p.push('{"a":1}\n{"b"');
  p.push(':2}\n\nnot json\n{"c":3}');
  assert.deepEqual(got, [{ a: 1 }, { b: 2 }]);
  assert.deepEqual(bad, ['not json']);
  p.push('\n');
  assert.deepEqual(got.at(-1), { c: 3 });
});

test('socket path: --socket beats HERDR_SOCKET_PATH beats the config default', () => {
  assert.equal(resolveSocketPath(['x', '--socket', '/a.sock'], { HERDR_SOCKET_PATH: '/b.sock' }), '/a.sock');
  assert.equal(resolveSocketPath(['x', '--socket=/c.sock'], {}), '/c.sock');
  assert.equal(resolveSocketPath(['x'], { HERDR_SOCKET_PATH: '/b.sock' }), '/b.sock');
  assert.equal(resolveSocketPath(['x'], { XDG_CONFIG_HOME: '/cfg' }), '/cfg/herdr/herdr.sock');
});

test('a live group keeps its views when herdr reports the same tab under another key', () => {
  const s = new GroupStore();
  const e = s.addEntry('w1:t2', { url: 'https://live/' });
  s.group('w1:t2').key = 'g-x';
  const live = s.group('w1:t2');
  s.dirty.clear();
  const out = s.applyHostState(hostState());
  assert.equal(s.group('w1:t2'), live);
  assert.deepEqual(live.entries.map((x) => x.view_id), [e.view_id]);
  assert.equal(live.key, 'g-aaa');
  assert.ok(s.dirty.has('w1:t2'));
  assert.deepEqual(out.created, []);
});

test('connect settles when herdr is down (missing socket) instead of hanging', async () => {
  const { Connection } = require('../src/herdr-api.js');
  const c = new Connection(require('node:path').join(require('node:os').tmpdir(), `hc-missing-${process.pid}.sock`));
  c.on('error', () => {});
  await assert.rejects(c.connect({ timeoutMs: 2000 }), (e) => e.code === 'ENOENT' || e.code === 'ECONNREFUSED');
});
