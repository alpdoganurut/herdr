'use strict';
// The placement feed: herdr's long poll and the companion's reports, against a stand-in
// link whose calls wait until the test answers them.
const test = require('node:test');
const assert = require('node:assert/strict');
const { PlacementFeed } = require('../src/dock/placements.js');

function fakeLink() {
  const calls = [];
  return {
    lease: { lease_id: 'l1', server_features: ['placements'] },
    calls,
    call(method, params) {
      return new Promise((resolve, reject) => calls.push({ method, params, resolve, reject }));
    },
    // the oldest unanswered call of `method`, answered or failed by the test
    take(method) {
      const call = calls.find((c) => c.method === method && !c.taken);
      assert.ok(call, `a ${method} call is out`);
      call.taken = true;
      return call;
    },
    count(method) {
      return calls.filter((c) => c.method === method).length;
    },
  };
}

// The feed's next steps run on promise jobs and setImmediate.
async function settle() {
  for (let i = 0; i < 4; i++) await new Promise((r) => setImmediate(r));
}

const info = (id) => ({ placement: { id, kind: 'side', tab_id: 't1', shown: true }, focused: true, foreground: true, updated_unix: 1 });

test('an answer to a poll from before stop() or a new lease is dropped', async () => {
  const link = fakeLink();
  const feed = new PlacementFeed({ link });
  const seen = [];
  feed.on('placements', (list) => seen.push(list));
  feed.start();
  const first = link.take('browser.placements');
  feed.stop();
  first.resolve({ boot_id: 'b1', rev: 1, placements: [info('p1')] });
  await settle();
  assert.equal(seen.length, 0, 'nothing emitted after stop()');
  assert.equal(feed.bootId, null);
  assert.equal(feed.rev, null);
  assert.equal(feed.polling, false);
  assert.equal(link.count('browser.placements'), 1, 'stopped: no next poll');
  // a new lease while a poll is out: its answer (the old server's) is dropped, the poll starts over
  feed.start();
  const second = link.take('browser.placements');
  feed.reset();
  second.resolve({ boot_id: 'old', rev: 9, placements: [info('p1')] });
  await settle();
  assert.equal(seen.length, 0);
  assert.equal(feed.bootId, null);
  const third = link.take('browser.placements');
  assert.equal(third.params.since_rev, undefined, 'from the start');
  third.resolve({ boot_id: 'b2', rev: 1, placements: [info('p1')] });
  await settle();
  assert.equal(seen.length, 1);
  assert.equal(feed.bootId, 'b2');
  // a failure of an outdated poll neither waits nor marks herdr as without placements
  const fourth = link.take('browser.placements');
  feed.reset();
  fourth.reject(Object.assign(new Error('gone'), { code: 'unknown_method' }));
  await settle();
  assert.equal(feed.unsupported, false);
  assert.ok(link.take('browser.placements'), 'polled again at once');
  feed.stop();
});

test('a placement that left the board and came back is reported again', async () => {
  const link = fakeLink();
  const feed = new PlacementFeed({ link });
  feed.start();
  link.take('browser.placements').resolve({ boot_id: 'b', rev: 1, placements: [info('p1')] });
  await settle();
  feed.report('p1', 'covering');
  feed.report('p1', 'covering');
  assert.equal(link.count('browser.placement_report'), 1, 'a report goes once');
  // undocked (herdr drops its report with the placement), then docked again
  link.take('browser.placements').resolve({ boot_id: 'b', rev: 2, placements: [] });
  await settle();
  link.take('browser.placements').resolve({ boot_id: 'b', rev: 3, placements: [info('p1')] });
  await settle();
  feed.report('p1', 'covering');
  assert.equal(link.count('browser.placement_report'), 2, 'reported anew');
  // it left and came back between two answers (rev 3 -> 5): reported anew too
  link.take('browser.placements').resolve({ boot_id: 'b', rev: 5, placements: [info('p1')] });
  await settle();
  feed.report('p1', 'covering');
  assert.equal(link.count('browser.placement_report'), 3);
  // the next change alone: no repeat
  link.take('browser.placements').resolve({ boot_id: 'b', rev: 6, placements: [info('p1')] });
  await settle();
  feed.report('p1', 'covering');
  assert.equal(link.count('browser.placement_report'), 3);
  // a new server (another boot): everything again
  link.take('browser.placements').resolve({ boot_id: 'b2', rev: 1, placements: [info('p1')] });
  await settle();
  feed.report('p1', 'covering');
  assert.equal(link.count('browser.placement_report'), 4);
  feed.stop();
});
