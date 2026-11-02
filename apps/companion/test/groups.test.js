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

test('unknown_tabs never drop groups (a moved tab keeps its views); a new lease resets seq and resyncs everything', () => {
  const s = new GroupStore();
  s.applyHostState(hostState());
  s.takeSync('L1');
  const live = s.group('w1:t2');
  const unknown = s.applySynced({ keys: {}, unknown_tabs: ['w1:t2', 'w9:gone'] });
  assert.deepEqual(unknown, ['w1:t2'], 'only tabs the companion has a group for');
  assert.equal(s.group('w1:t2'), live, 'the group stays until host_state says otherwise');
  // the re-read follows the key to the tab's new id: same group, same entries
  const out = s.applyHostState({ focused_tab_id: 'w2:t5', tabs: [{ tab_id: 'w2:t5', workspace_id: 'w2', label: 'builder', key: 'g-aaa' }] });
  assert.deepEqual(out.moved, [{ from: 'w1:t2', to: 'w2:t5' }]);
  assert.deepEqual(out.dropped, []);
  assert.equal(s.group('w2:t5'), live);
  assert.ok(s.dirty.has('w2:t5'), 'resynced under the new id');
  s.addEntry('w1:t1', { url: 'https://x/' });
  s.takeSync('L1');
  s.resetLease();
  const p = s.takeSync('L2');
  assert.equal(p.seq, 1);
  assert.deepEqual(p.groups.map((g) => g.tab_id).sort(), ['w1:t1', 'w2:t5'], 'every group again');
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

test('a sync over the byte budget is split by whole groups; the rest stay dirty with the next seq', () => {
  const s = new GroupStore();
  const long = 'https://x.test/' + 'a'.repeat(2000);
  for (const t of ['t1', 't2', 't3', 't4', 't5']) for (let i = 0; i < 32; i++) s.addEntry(t, { url: long + i, title: 'T'.repeat(300) });
  const budget = 256 * 1024 - 8 * 1024;
  const sent = [];
  let p;
  while ((p = s.takeSync('L1', { maxBytes: budget }))) {
    assert.ok(Buffer.byteLength(JSON.stringify(p.groups)) <= budget, `seq ${p.seq} fits`);
    sent.push(p);
  }
  assert.ok(sent.length >= 2, 'split into several syncs');
  assert.deepEqual(sent.map((x) => x.seq), sent.map((_, i) => i + 1), 'each sync its own seq');
  assert.deepEqual(sent.flatMap((x) => x.groups.map((g) => g.tab_id)).sort(), ['t1', 't2', 't3', 't4', 't5']);
  assert.equal(s.dirty.size, 0);
  // one group alone over the budget: sent alone, titles cut
  const one = s.takeSync('L1', { all: true, maxBytes: 1000 });
  assert.equal(one.groups.length, 1);
  assert.ok(one.groups[0].entries.every((e) => e.title.length <= 80));
  assert.equal(s.dirty.size, 4, 'the other groups wait');
});

test('an explicit title is never replaced by the page title', () => {
  const s = new GroupStore();
  const fixed = s.addEntry('t', { kind: 'html_view', file: '/r/a.html', url: 'file:///r/a.html', title: 'Report', title_fixed: true });
  const plain = s.addEntry('t', { url: 'https://p/', title: 'p' });
  s.updateEntry(fixed.view_id, { title: 'from <title>', url: 'file:///r/a.html#x' });
  s.updateEntry(plain.view_id, { title: 'from <title>' });
  assert.equal(fixed.title, 'Report');
  assert.equal(fixed.url, 'file:///r/a.html#x', 'other fields still update');
  assert.equal(plain.title, 'from <title>');
});

test('media grants are per type: a microphone grant is not a camera grant', () => {
  const { grantKeys } = require('../src/views.js');
  const mic = grantKeys('https://o.test', 'media', { mediaTypes: ['audio'] });
  const cam = grantKeys('https://o.test', 'media', { mediaTypes: ['video'] });
  const both = grantKeys('https://o.test', 'media', { mediaTypes: ['video', 'audio'] });
  const grants = new Set(mic);
  assert.ok(!cam.every((k) => grants.has(k)), 'camera still asks');
  assert.ok(!both.every((k) => grants.has(k)), 'camera + mic still asks');
  assert.ok(grantKeys('https://o.test', 'media', { mediaType: 'audio' }).every((k) => grants.has(k)), 'the mic check passes');
  assert.ok(!grantKeys('https://o.test', 'media', { mediaType: 'video' }).every((k) => grants.has(k)), 'the camera check fails');
  assert.deepEqual(grantKeys('https://o.test', 'geolocation'), ['https://o.test|geolocation']);
});

test('an html view of a symlinked file reads the directory the file really lives in', () => {
  const fs = require('node:fs');
  const os = require('node:os');
  const path = require('node:path');
  const { viewRoot, viewPartition } = require('../src/views.js');
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'hc-root-')));
  fs.mkdirSync(path.join(d, 'real'));
  fs.mkdirSync(path.join(d, 'links'));
  fs.writeFileSync(path.join(d, 'real', 'a.html'), 'x');
  fs.symlinkSync(path.join(d, 'real', 'a.html'), path.join(d, 'links', 'a.html'));
  assert.equal(viewRoot(path.join(d, 'links'), path.join(d, 'links', 'a.html')), path.join(d, 'real'));
  assert.equal(viewRoot(d, path.join(d, 'real', 'a.html')), d, 'a root that holds the file stays');
  assert.equal(viewPartition('/r', false), viewPartition('/r', false), 'views of one directory share a session');
  assert.notEqual(viewPartition('/r', false), viewPartition('/r', true));
  fs.rmSync(d, { recursive: true, force: true });
});

test('serialized runs one call at a time in order; a failure does not block the next', async () => {
  const { serialized } = require('../src/herdr-api.js');
  const log = [];
  let active = 0;
  const errors = [];
  const run = serialized(async (n, ms) => {
    active++;
    assert.equal(active, 1, 'never two at once');
    log.push(`start ${n}`);
    await new Promise((r) => setTimeout(r, ms));
    active--;
    log.push(`end ${n}`);
    if (n === 2) throw new Error('two failed');
    return n;
  }, (e) => errors.push(e.message));
  // the slow first read would answer last if they overlapped
  const results = await Promise.allSettled([run(1, 40), run(2, 5), run(3, 1)]);
  assert.deepEqual(log, ['start 1', 'end 1', 'start 2', 'end 2', 'start 3', 'end 3']);
  assert.deepEqual(results.map((r) => r.status), ['fulfilled', 'rejected', 'fulfilled']);
  assert.equal(results[2].value, 3);
  assert.deepEqual(errors, ['two failed']);
});
