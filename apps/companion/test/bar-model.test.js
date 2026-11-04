'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const bar = require('../src/ui/bar-model.js');

test('a file:// page shows its project folder, the file and the fragment', () => {
  assert.equal(
    bar.compactUrl('file:///Users/me/Projects/Leap/HiddenStoryAi/search-it/.leap/work/drafts/aws-costs/aws-costs.html#cdn'),
    'search-it › aws-costs.html #cdn',
  );
  // no dot folder: the file's own folder
  assert.equal(bar.compactUrl('file:///tmp/site/report.html'), 'site › report.html');
  // percent-escapes read as text
  assert.equal(bar.compactUrl('file:///tmp/my%20site/a%20b.html'), 'my site › a b.html');
  // a dot folder at the root: just the file
  assert.equal(bar.compactUrl('file:///.cache/x.html'), 'x.html');
  assert.equal(bar.compactUrl('file:///x.html'), 'x.html');
});

test('an http(s) page shows its host and the last path segment', () => {
  assert.equal(bar.compactUrl('https://www.example.com/d/abc/cdn-egress?orgId=1#panel'), 'example.com › cdn-egress #panel');
  assert.equal(bar.compactUrl('http://localhost:3000/'), 'localhost:3000');
  assert.equal(bar.compactUrl('http://127.0.0.1:8080/a/b/'), '127.0.0.1:8080 › b');
});

test('blank and other urls', () => {
  assert.equal(bar.compactUrl(''), '');
  assert.equal(bar.compactUrl(null), '');
  assert.equal(bar.compactUrl('about:blank'), '');
  assert.equal(bar.compactUrl('data:text/html,hi'), 'data:text/html,hi');
  assert.equal(bar.compactUrl('not a url'), 'not a url');
});

test('the bar wraps below 560 px', () => {
  assert.equal(bar.isNarrow(420), true);
  assert.equal(bar.isNarrow(559), true);
  assert.equal(bar.isNarrow(560), false);
  assert.equal(bar.isNarrow(900), false);
  assert.equal(bar.isNarrow(0), false, 'not laid out yet');
});

test('the surface and its pin: docked and Browser view unpinned, floating pinned, saved per surface', () => {
  assert.equal(bar.surface(false, 'side'), 'floating');
  assert.equal(bar.surface(true, 'side'), 'docked');
  assert.equal(bar.surface(true, 'full'), 'full');
  assert.equal(bar.pinnedFor({}, 'docked'), false);
  assert.equal(bar.pinnedFor({}, 'full'), false);
  assert.equal(bar.pinnedFor({}, 'floating'), true);
  assert.equal(bar.pinnedFor(null, 'floating'), true);
  const saved = { docked: true, floating: false };
  assert.equal(bar.pinnedFor(saved, 'docked'), true);
  assert.equal(bar.pinnedFor(saved, 'full'), false);
  assert.equal(bar.pinnedFor(saved, 'floating'), false);
  assert.equal(bar.pinnedFor({ docked: 'yes' }, 'docked'), false, 'not a boolean: the default');
});

test('expand / restore and close only docked, only with placement_actions', () => {
  const features = new Set(['placements', 'placement_actions']);
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'side', features, placementId: 'pl-1', clientTakes: true }), { expand: 'promote', close: true });
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'full', features, placementId: 'pl-1', clientTakes: true }), { expand: 'restore', close: true });
  const none = { expand: null, close: false };
  assert.deepEqual(bar.windowActions({ docked: false, kind: 'side', features, placementId: 'pl-1', clientTakes: true }), none, 'floating: the traffic lights');
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'side', features: new Set(['placements']), placementId: 'pl-1', clientTakes: true }), none, 'an older herdr');
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'side', features: null, placementId: 'pl-1', clientTakes: true }), none);
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'side', features, placementId: null, clientTakes: true }), none);
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'side', features: ['placement_actions'], placementId: 'pl-1', clientTakes: true }), { expand: 'promote', close: true });
});

test('an older herdr client (it ignores the actions): neither button', () => {
  const features = new Set(['placements', 'placement_actions']);
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'side', features, placementId: 'pl-1', clientTakes: false }), { expand: null, close: false });
  assert.deepEqual(bar.windowActions({ docked: true, kind: 'side', features, placementId: 'pl-1' }), { expand: null, close: false });
});

test('the hidden bar is held open by what needs the user', () => {
  assert.equal(bar.held({}), false);
  for (const k of ['urlFocus', 'menu', 'sendbar', 'permbar', 'status', 'needsYou', 'empty']) assert.equal(bar.held({ [k]: true }), true, k);
});
