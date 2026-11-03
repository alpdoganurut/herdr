// The companion's view of herdr's sessions (pure): the bar's count pill and its menu.
// A session is a herdr tab; its pages are its group's entries (placeholders included:
// restored pages that were not loaded yet still count).
'use strict';

const ATTENTION = '◉'; // ◉ a page needs the user
const MARK = '◎'; // ◎

// {here, total, sessions}: pages in `shownTab`'s session, pages in every session, and how
// many sessions have pages.
function sessionCounts(store, shownTab) {
  let total = 0;
  let sessions = 0;
  for (const g of store.groups.values()) {
    const n = g.entries.length;
    if (!n) continue;
    total += n;
    sessions += 1;
  }
  const shown = shownTab ? store.group(shownTab) : null;
  return { here: shown ? shown.entries.length : 0, total, sessions };
}

// The pill's hover text: "3 pages in this session · 12 pages in 5 sessions".
function countsTitle({ here, total, sessions }) {
  const pages = (n) => `${n} page${n === 1 ? '' : 's'}`;
  return `${pages(here)} in this session · ${pages(total)} in ${sessions} session${sessions === 1 ? '' : 's'}`;
}

// One menu item per session with pages, herdr's tab order kept (the store's order):
// {tab_id, label: "◉ label", pages: "3 pages", current}.
function sessionMenuItems(store, shownTab) {
  const out = [];
  for (const g of store.groups.values()) {
    const n = g.entries.length;
    if (!n) continue;
    const attention = g.entries.some((e) => e.attention);
    out.push({
      tab_id: g.tab_id,
      label: `${attention ? ATTENTION : MARK}  ${store.label(g.tab_id)}`,
      pages: `${n} page${n === 1 ? '' : 's'}`,
      current: g.tab_id === shownTab,
    });
  }
  return out;
}

module.exports = { sessionCounts, sessionMenuItems, countsTitle };
