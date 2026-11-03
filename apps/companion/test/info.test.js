'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { GroupStore } = require('../src/groups.js');
const { sessionCounts, sessionMenuItems, countsTitle } = require('../src/info.js');

function store() {
  const s = new GroupStore();
  s.applyHostState({
    focused_tab_id: 'w1:t1',
    tabs: [
      { tab_id: 'w1:t1', workspace_id: 'w1', label: 'planner', key: 'g-a', entries: [{ id: 'b1', kind: 'web', url: 'https://a.test/' }, { id: 'b2', kind: 'web', url: 'https://b.test/' }, { id: 'b3', kind: 'web', url: 'https://c.test/' }] },
      { tab_id: 'w1:t2', workspace_id: 'w1', label: 'builder' },
      { tab_id: 'w2:t1', workspace_id: 'w2', label: 'reviewer', key: 'g-b', entries: [{ id: 'b1', kind: 'web', url: 'https://d.test/' }] },
    ],
  });
  return s;
}

test('the pill counts this session, every session and the sessions with pages', () => {
  const s = store();
  assert.deepEqual(sessionCounts(s, 'w1:t1'), { here: 3, total: 4, sessions: 2 });
  assert.deepEqual(sessionCounts(s, 'w1:t2'), { here: 0, total: 4, sessions: 2 });
  assert.equal(countsTitle({ here: 1, total: 4, sessions: 2 }), '1 page in this session · 4 pages in 2 sessions');
});

test('the sessions menu lists sessions with pages, marks the shown one and a page that needs you', () => {
  const s = store();
  s.group('w2:t1').entries[0].attention = true;
  const items = sessionMenuItems(s, 'w1:t1');
  assert.deepEqual(
    items.map((i) => [i.tab_id, i.label, i.pages, i.current]),
    [
      ['w1:t1', '◎  planner', '3 pages', true],
      ['w2:t1', '◉  reviewer', '1 page', false],
    ],
  );
});
