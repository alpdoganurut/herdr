'use strict';
// What a remote companion takes from the other machine (src/remote-rules.js): text, URLs, rule 3,
// a shown file through the helper, the link's state (src/host.js), its groups (src/groups.js).
// The live run is test/e2e-remote-client.mjs (files, show, link, labels).
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const rules = require('../src/remote-rules.js');
const { GroupStore } = require('../src/groups.js');
const { HerdrLink, EVENT_KINDS } = require('../src/host.js');
const { remoteLine, compactUrl } = require('../src/ui/bar-model.js');

const T1 = 'k3v9x0q2m7w1z8r4t6y5u0ab';
const T2 = 'a'.repeat(24);
const T3 = 'b'.repeat(24);

test('text: one line, no control, bidi or invisible characters, clipped', () => {
  assert.equal(rules.cleanText('dev\u001b[1mhost1\n'), 'dev[1mhost1');
  assert.equal(rules.cleanText('a\r\nb\tc\u2028d'), 'a b c d');
  assert.equal(rules.cleanText('x\u202ey\u2066z\u200b\u200f\ufeff\u061c\u0000\u009b'), 'xyz');
  assert.equal(rules.cleanText('é ü 日本 👩‍💻'), 'é ü 日本 👩‍💻');
  assert.equal(rules.cleanText('a'.repeat(999)).length, 300);
  assert.equal(rules.cleanText('abc', 2), 'ab');
  for (const v of [null, undefined, 5, {}, ['a']]) assert.equal(rules.cleanText(v), '');
});

test('URLs: http(s) in any case, about:blank alone', () => {
  for (const u of ['http://a.test/', 'HTTPS://a.test/x?y#z', 'about:blank']) assert.ok(rules.isWebUrl(u), u);
  for (const u of ['', 'about:blank#x', 'about:srcdoc', 'file:///etc/hosts', 'herdr-file://x/y', 'javascript:1', 'data:text/html,x', 'chrome://gpu', 'http:/a', ' http://a', 'http://a/\n', 'http://a/\u0085', null, 5]) assert.ok(!rules.isWebUrl(u), String(u));
});

test('tokens and paths: the gate\'s and the sidecar\'s rule', () => {
  for (const t of [T1, T2]) assert.ok(rules.isFileToken(t));
  for (const t of ['', 'K' + T1.slice(1), '1'.repeat(24), T1.slice(1), T1 + 'a', T1.slice(0, 23) + '/', null, 5]) assert.ok(!rules.isFileToken(t), String(t));
  for (const p of ['index.html', 'report/in%20dex.html', 'a b/ü.css', '...', 'a/.hidden', '%2e%2e%2e', '%252e%252e/x']) assert.ok(rules.isFileRel(p), p);
  for (const p of ['', '/etc/hosts', '../x.html', 'a/../x.html', 'a/%2e%2E/x.html', './x.html', 'a/%2e/x', 'a//b', 'a/', 'a\\x.html', 'a%5cx.html', 'a%2fx.html', 'x%00.html', 'x.html?y', 'x.html#y', 'x\n', 'x\u0000', 'x\u007f', null, 5]) assert.ok(!rules.isFileRel(p), String(p));
});

test('a shown file\'s URL: parsed, and the same page under another token', () => {
  assert.deepEqual(rules.parseFileUrl(`herdr-file://${T1}/a/b.html?x=1#y?z`), { token: T1, rel: 'a/b.html', suffix: '?x=1#y?z' });
  for (const u of [`herdr-file://${T1}`, `herdr-file://${T1.toUpperCase()}/a`, `herdr-file://u@${T1}/a`, `herdr-file://${T1}:80/a`, 'file:///a', '']) assert.equal(rules.parseFileUrl(u), null, u);
  assert.equal(rules.retoken(`herdr-file://${T1}/a?b#c`, T1, T2), `herdr-file://${T2}/a?b#c`);
  for (const [u, from, to] of [[`herdr-file://${T1}/a`, T3, T2], ['http://a/', T1, T2], [`herdr-file://${T1}/a`, T1, 'x'], [`herdr-file://${T1}/a`, T1, null]]) assert.equal(rules.retoken(u, from, to), u);
});

const web = (id, url, more = {}) => ({ id, kind: 'web', url, title: id, opened_by: 'agent:w1:p1', ...more });
const html = (id, serve, url, more = {}) => ({ id, kind: 'html_view', serve, url, title: id, opened_by: 'agent:w1:p1', ...more });
const stateOf = (entries, more = {}) => ({ focused_tab_id: 't1', tabs: [{ tab_id: 't1', workspace_id: 'w1', label: 'one', key: 'g1', active: entries[0] && entries[0].id, entries, ...more }] });

test('rule 3: restored pages are web pages and shown files with their own token', () => {
  const s = new GroupStore({ remote: true });
  s.applyHostState(stateOf([
    web('b1', 'https://a.test/x'), web('b2', 'file:///etc/passwd'), web('b3', `herdr-file://${T1}/x.html`, { serve: T1 }), web('b4', 'chrome://gpu'), web('b5', 'about:blank'),
    html('b6', T1, `herdr-file://${T1}/a/x.html#s`, { file: '/etc/passwd', allow_network: true, page_api: true, opener: 'o' }),
    html('b7', undefined, 'file:///b/x.html', { file: '/b/x.html' }), html('b8', 'UP', 'herdr-file://UP/x.html'), html('b9', T1, `herdr-file://${T2}/x.html`),
    html('b10', T1, `herdr-file://${T1}/a%2f..%2fx`), html('b11', T1, 'file:///b/x.html'), html('b12', null, 'about:blank'), null, { id: 'b13', kind: 'other', url: 'about:srcdoc' },
  ]));
  const got = s.group('t1').entries.map((e) => [e.id, e.kind, e.url]);
  assert.deepEqual(got, [
    ['b1', 'web', 'https://a.test/x'], ['b2', 'web', 'about:blank'], ['b3', 'web', 'about:blank'], ['b4', 'web', 'about:blank'], ['b5', 'web', 'about:blank'],
    ['b6', 'html_view', `herdr-file://${T1}/a/x.html#s`], ['b9', 'html_view', 'about:blank'], ['b10', 'html_view', 'about:blank'], ['b11', 'html_view', 'about:blank'], ['b13', 'web', 'about:blank'],
  ]);
  const e = s.group('t1').entries.find((x) => x.id === 'b6');
  assert.deepEqual([e.file, e.root, e.allow_network, e.page_api, e.opener, e.serve], [null, null, false, false, null, T1]);
  for (const x of s.group('t1').entries) if (x.kind === 'web') assert.equal(x.serve, null);
  // a group that keeps nothing is none; its first page otherwise stands in for a dropped active one
  const none = new GroupStore({ remote: true });
  assert.deepEqual(none.applyHostState(stateOf([html('b1', null, 'file:///x')])).created, []);
  assert.equal(none.group('t1'), null);
  const next = new GroupStore({ remote: true });
  next.applyHostState(stateOf([html('b1', null, 'file:///x'), web('b2', 'http://a/')]));
  assert.equal(next.group('t1').active, next.group('t1').entries[0].view_id);
});

test('another machine\'s text is cleaned where it is kept; a local herdr\'s is not touched', () => {
  const s = new GroupStore({ remote: true });
  s.applyHostState(stateOf([web('b1', 'http://a/', { title: 'x\u202e\u0007y', opened_by: 'agent:\u001b' + 'w'.repeat(200) })], { label: 'on\u202ee\n', workspace_id: 'w\u00001' }));
  assert.deepEqual(s.tabs.get('t1'), { tab_id: 't1', workspace_id: 'w1', label: 'one' });
  const e = s.group('t1').entries[0];
  assert.deepEqual([e.title, e.opened_by.length], ['xy', 80]);
  s.updateEntry(e.view_id, { title: 'a\u2028b' });
  assert.equal(e.title, 'a b');
  assert.equal(s.addEntry('t1', { title: 'c\u202ed' }).title, 'cd');
  const local = new GroupStore();
  local.applyHostState(stateOf([web('b1', 'file:///x', { title: 'x\u202ey' }), html('b2', undefined, 'file:///b/x.html', { file: '/b/x.html', allow_network: true })], { label: 'on\u202ee' }));
  assert.equal(local.tabs.get('t1').label, 'on\u202ee');
  assert.deepEqual(local.group('t1').entries.map((x) => [x.title, x.url, x.file, x.root, x.allow_network]), [['x\u202ey', 'file:///x', null, null, false], ['b2', 'file:///b/x.html', '/b/x.html', '/b', true]]);
  assert.deepEqual(local.syncGroup(local.group('t1')).entries[1], { view_id: 'v2', kind: 'html_view', url: 'file:///b/x.html', title: 'b2', id: 'b2', file: '/b/x.html', allow_network: true, opened_by: 'agent:w1:p1' });
});

test('a tab without a name is named by its id, cleaned; the id itself stays as herdr sent it', () => {
  const id = 'w1:t\u202e1\u0007';
  const s = new GroupStore({ remote: true });
  s.applyHostState({ tabs: [{ tab_id: id, label: '\u0007', workspace_id: 'w1' }] });
  assert.deepEqual(s.tabs.get(id), { tab_id: id, workspace_id: 'w1', label: 'w1:t1' });
  assert.equal(s.label(id), 'w1:t1');
  const local = new GroupStore();
  local.applyHostState({ tabs: [{ tab_id: id, workspace_id: 'w1' }] });
  assert.equal(local.label(id), id);
});

test('a new host_state renews the tokens: of a live page (its URL stays), of one not loaded yet', () => {
  const s = new GroupStore({ remote: true });
  s.applyHostState(stateOf([html('b1', T1, `herdr-file://${T1}/x.html`), html('b2', T2, `herdr-file://${T2}/y.html`)]));
  const [live, cold] = s.group('t1').entries;
  live.page_token = live.serve; // views.create
  s.updateEntry(live.view_id, { url: `herdr-file://${T1}/x.html#two` });
  s.applyHostState(stateOf([html('b1', T3, `herdr-file://${T3}/x.html`), html('b2', T3, `herdr-file://${T3}/y.html`)]));
  assert.deepEqual([live.url, live.serve], [`herdr-file://${T1}/x.html#two`, T3]);
  assert.deepEqual([cold.url, cold.serve], [`herdr-file://${T3}/y.html`, T3]);
  assert.equal(s.serveOf(T1), T3);
  assert.equal(s.serveOf(T2), null);
  assert.equal(s.wireUrl(live), `herdr-file://${T3}/x.html#two`);
  assert.deepEqual(s.syncGroup(s.group('t1')).entries[0], { view_id: live.view_id, kind: 'html_view', url: `herdr-file://${T3}/x.html#two`, title: 'b1', id: 'b1', serve: T3, opened_by: 'agent:w1:p1' });
  // no or a bad token in the new state: the old one stays
  s.applyHostState(stateOf([html('b1', null, ''), html('b2', 'X', '')]));
  assert.deepEqual([live.serve, cold.serve], [T3, T3]);
  // the link is back: everything again, the seq goes on
  s.takeSync('l');
  assert.equal(s.takeSync('l'), null);
  s.markAll();
  const again = s.takeSync('l');
  assert.equal(again.seq, 2);
  assert.equal(again.groups.length, 1);
});

test('page_api is never kept for another machine\'s page', () => {
  const s = new GroupStore({ remote: true });
  s.applyHostState({ tabs: [{ tab_id: 't1' }] });
  const e = s.addEntry('t1', { kind: 'html_view', serve: T1, url: `herdr-file://${T1}/x`, page_api: true, opened_by: 'agent:w1:p1' });
  assert.equal(e.page_api, false);
  assert.equal(new GroupStore().addEntry('t1', { kind: 'html_view', page_api: true }).page_api, true);
});

// ---------- link.file ----------

const req = (url, method = 'GET') => ({ url, method });
const u = (rel) => `herdr-file://${T1}/${rel}`;
const b64 = (s) => Buffer.from(s).toString('base64');
async function answer(request, pull, more = {}) {
  const asked = [];
  const res = await rules.fileResponse(request, { token: T1, pull: async (rel, offset) => (asked.push([rel, offset]), pull(rel, offset)), down: 'devhost1: down', ...more });
  const body = await res.text().then((t) => t, (e) => ({ error: e.message }));
  return { status: res.status, headers: Object.fromEntries(res.headers), body, asked };
}

test('a file: pulled piece by piece until eof', async () => {
  const parts = ['hello ', 'wor', 'ld'];
  const pull = (_rel, offset) => {
    const i = { 0: 0, 6: 1, 9: 2 }[offset];
    return { status: 200, mime: 'text/css', size: 11, data: b64(parts[i]), eof: i === 2 };
  };
  const r = await answer(req(u('a%20b/c.css?v=2#x')), pull);
  assert.deepEqual([r.status, r.body, r.asked], [200, 'hello world', [['a%20b/c.css', 0], ['a%20b/c.css', 6], ['a%20b/c.css', 9]]]);
  assert.deepEqual(r.headers, { 'content-type': 'text/css', 'content-length': '11', 'cache-control': 'no-store' });
  const head = await answer(req(u('x'), 'HEAD'), pull);
  assert.deepEqual([head.status, head.body, head.headers['content-length'], head.asked.length], [200, '', '11', 1]);
  const empty = await answer(req(u('x')), () => ({ status: 200, mime: '', size: 0, data: '', eof: true }));
  assert.deepEqual([empty.status, empty.body, empty.headers['content-type']], [200, '', 'application/octet-stream']);
});

test('refused without asking: another token, a bad path, another method', async () => {
  const no = () => assert.fail('asked');
  for (const [r, status] of [[req(`herdr-file://${T2}/x`), 403], [req('http://a/'), 403], [req(u('')), 404], [req(u('a%2fb')), 404], [req(u('../x')), 404], [req(u('a%00')), 404], [req(u('x'), 'POST'), 405], [req(u('x'), 'PUT'), 405]]) {
    const got = await answer(r, no);
    assert.deepEqual([got.status, got.asked.length], [status, 0], r.url);
  }
});

test('the other machine\'s refusals are passed on; no link is 502 with the machine\'s name', async () => {
  for (const status of [403, 404, 413]) assert.equal((await answer(req(u('x')), () => ({ status }))).status, status);
  for (const status of [204, 301, 399]) assert.equal((await answer(req(u('x')), () => ({ status }))).status, 502);
  const down = await answer(req(u('x')), () => Promise.reject(new Error('link_down')));
  assert.deepEqual([down.status, down.body, down.headers['content-type']], [502, 'devhost1: down', 'text/plain; charset=utf-8']);
});

test('nonsense is 502; a file that breaks off or changes ends in an error, never in a short page', async () => {
  const one = { status: 200, mime: 'text/html', size: 4, data: b64('ab'), eof: false };
  for (const bad of [null, {}, { status: '200' }, { status: 9 }, { ...one, size: -1 }, { ...one, size: 1.5 }, { ...one, size: rules.FILE_MAX + 1 }, { ...one, data: 5 }, { ...one, size: 1 }, { ...one, data: '' }, { ...one, eof: true }]) {
    assert.equal((await answer(req(u('x')), () => bad)).status, 502, JSON.stringify(bad));
  }
  for (const next of [{ ...one, size: 5 }, { ...one, data: '' }, { ...one, data: b64('abc') }, { ...one, data: b64('a'), eof: true }, { status: 403 }, null]) {
    const got = await answer(req(u('x')), (_r, o) => (o === 0 ? one : next));
    assert.ok(got.body.error, JSON.stringify(next));
  }
  assert.ok((await answer(req(u('x')), (_r, o) => (o === 0 ? one : Promise.reject(new Error('gone'))))).body.error);
  // a header never takes a line break
  assert.equal((await answer(req(u('x')), () => ({ ...one, mime: 'a/b\r\nx: y', eof: true, size: 2 }))).headers['content-type'], 'application/octet-stream');
});

test('at most 16 pulls on the way, in order, also after a failure', async () => {
  const lanes = new rules.Lanes();
  let now = 0;
  let peak = 0;
  const order = [];
  const runs = [...Array(40).keys()].map((i) =>
    lanes.run(async () => {
      order.push(i);
      peak = Math.max(peak, ++now);
      await new Promise((r) => setImmediate(r));
      now--;
      if (i % 5 === 0) throw new Error('x');
      return i;
    }),
  );
  const done = await Promise.allSettled(runs);
  assert.equal(peak, rules.FILE_PULLS);
  assert.deepEqual(order, [...Array(40).keys()]);
  assert.equal(done.filter((d) => d.status === 'rejected').length, 8);
  assert.equal(await lanes.run(async () => 'free'), 'free');
});

// ---------- the link's state ----------

test('the lease\'s link: known states only, its text cleaned', () => {
  assert.deepEqual(rules.linkState({ state: 'linked' }), { state: 'linked', text: null });
  assert.deepEqual(rules.linkState({ state: 'in_use', text: 'a\u202eb\n' }), { state: 'in_use', text: 'ab' });
  for (const v of [null, undefined, {}, { state: 'other' }, 'linked']) assert.equal(rules.linkState(v), null);
});

test('the bar\'s line: the helper, the link, the page', () => {
  const reg = { state: 'registered', text: 'attached as companion' };
  const m = { machine: 'devhost1', status: reg, link: { state: 'linked', text: null }, page: null };
  assert.equal(remoteLine(m), null);
  assert.equal(remoteLine({ ...m, link: null }), null);
  assert.equal(remoteLine({ ...m, status: { state: 'connecting', text: 'connecting to herdr…' }, page: 'waits' }), 'devhost1: connecting to herdr…');
  assert.equal(remoteLine({ ...m, link: { state: 'reconnecting', text: null }, page: 'failed' }), 'devhost1: reconnecting');
  assert.equal(remoteLine({ ...m, link: { state: 'in_use', text: 'the browser of devhost1 is on laptop1 (since 14:02); quit herdr+ there' } }), 'the browser of devhost1 is on laptop1 (since 14:02); quit herdr+ there');
  assert.equal(remoteLine({ ...m, link: { state: 'in_use', text: null } }), 'the browser of devhost1 is on another Mac; quit herdr+ there');
  assert.match(remoteLine({ ...m, page: 'waits' }), /^devhost1: no connection for this page; it loads/);
  assert.match(remoteLine({ ...m, page: 'failed' }), /^devhost1: this page did not load/);
});

test('the bar\'s address of a shown file: folder and name, no token', () => {
  assert.equal(compactUrl(`herdr-file://${T1}/work/rep%20ort/x.html#cdn`), 'rep ort › x.html #cdn');
  assert.equal(compactUrl(`herdr-file://${T1}/x.html?y=1`), 'x.html');
});

// A helper on a real socket: `state` is what its answers come from.
async function helper(t, state) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hc-far-'));
  const sock = path.join(dir, 's.sock');
  const streams = [];
  const server = net.createServer((s) => {
    s.on('error', () => {});
    s.once('data', (d) => {
      const r = JSON.parse(String(d));
      state.asked.push(r);
      const ok = (result) => JSON.stringify({ id: r.id, result }) + '\n';
      if (r.method === 'events.subscribe') return streams.push(s), s.write(ok({}));
      const relayed = !/register|heartbeat/.test(r.method);
      if (relayed && state.down) return s.end(JSON.stringify({ id: r.id, error: { code: 'link_down', message: 'down' } }) + '\n');
      s.end(ok(relayed ? { state: {} } : { lease: { lease_id: 'L1', profile: 'companion', heartbeat_ms: 500, ...(state.link ? { link: state.link } : {}) } }));
    });
  });
  await new Promise((r) => server.listen(sock, r));
  const link = new HerdrLink({ socketPath: sock, register: () => ({}), remote: state.remote !== false });
  t.after(async () => {
    await link.stop();
    for (const s of streams) s.destroy();
    await new Promise((r) => server.close(r));
    fs.rmSync(dir, { recursive: true, force: true });
  });
  const events = [];
  const links = [];
  link.on('event', (e) => events.push(e.event));
  link.on('link', (l) => links.push(l.state));
  link.start();
  await new Promise((r) => link.once('lease', r));
  link.releaseEvents();
  const until = async (f) => {
    for (let i = 0; i < 150 && !f(); i++) await new Promise((r) => setTimeout(r, 20));
    assert.ok(f(), String(f));
  };
  return { link, events, links, until, push: (e) => streams.forEach((s) => s.write(JSON.stringify(e) + '\n')) };
}
const UP = { event: 'herdr.link_up', data: {} };

test('link_down marks the link, herdr.link_up brings it back; the lease stays', async (t) => {
  const state = { asked: [], down: false, link: null };
  const h = await helper(t, state);
  assert.equal(h.link.down, false);
  state.down = true;
  await assert.rejects(h.link.call('browser.host_state'), { code: 'link_down' });
  assert.equal(h.link.down, true);
  await assert.rejects(h.link.request('link.file', { token: T1, path: 'x', offset: 0 }), { code: 'link_down' });
  assert.deepEqual(h.links, ['reconnecting'], 'told once');
  state.down = false;
  h.push(UP);
  await h.until(() => h.events.length === 1);
  assert.deepEqual([h.link.down, h.events, h.links], [false, ['herdr.link_up'], ['reconnecting', 'linked']]);
  assert.equal(state.asked.filter((r) => r.method === 'browser.host_register').length, 1);
  // not a herdr event kind: the helper's own, on the same stream
  assert.ok(!EVENT_KINDS.includes('herdr.link_up'));
  assert.deepEqual(state.asked.find((r) => r.method === 'events.subscribe').params.subscriptions.map((x) => x.type), EVENT_KINDS);
});

test('a link_down that comes after the event: an answer over the link, or the next heartbeat, brings it back', async (t) => {
  const state = { asked: [], down: false, link: null };
  const h = await helper(t, state);
  h.push(UP);
  await h.until(() => h.events.length === 1);
  const late = async () => {
    state.down = true;
    await assert.rejects(h.link.call('browser.host_state'), { code: 'link_down' });
    state.down = false;
    assert.equal(h.link.down, true);
  };
  await late();
  await h.link.request('link.file', { token: T1, path: 'x', offset: 0 });
  assert.deepEqual([h.link.down, h.events.length], [false, 2], 'and everything is synced again');
  await late();
  await h.until(() => !h.link.down); // nobody asks: the heartbeat does (the lease says nothing)
  assert.equal(h.events.length, 3);
  await new Promise((r) => setTimeout(r, 1200));
  assert.equal(h.events.length, 3);
});

test('the lease says how the link is: another Mac; back by a heartbeat alone is link_up', async (t) => {
  const state = { asked: [], down: true, link: { state: 'in_use', text: 'the browser of devhost1 is on laptop1' } };
  const h = await helper(t, state);
  assert.deepEqual(h.link.farLink, { state: 'in_use', text: 'the browser of devhost1 is on laptop1' });
  await assert.rejects(h.link.call('browser.host_state'), { code: 'link_down' });
  assert.equal(h.link.farLink.state, 'in_use', 'link_down does not hide which');
  state.link = { state: 'linked' };
  await h.until(() => h.events.length === 1);
  assert.deepEqual([h.link.down, h.events], [false, ['herdr.link_up']]);
  state.link = { state: 'linked' };
  await new Promise((r) => setTimeout(r, 1200));
  assert.equal(h.events.length, 1, 'once');
});

test('a local herdr: no link state, whatever it answers', async (t) => {
  const state = { asked: [], down: true, link: { state: 'in_use' }, remote: false };
  const h = await helper(t, state);
  await assert.rejects(h.link.call('browser.host_state'), { code: 'link_down' });
  assert.deepEqual([h.link.down, h.links], [false, []]);
});
