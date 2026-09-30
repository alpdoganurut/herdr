// The herdr+ new tab page: renders the snapshot herdr pushes into the
// worker's session storage (`snapshot`), live through storage.onChanged.
// Every title, URL and label is untrusted page text: textContent only.
'use strict';

const SNAPSHOT_VERSION = 1;
const body = document.getElementById('body');
let snapshot = null;
let tabsCache = null;

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}
function ago(at, now) {
  if (!at) return '';
  const s = Math.max(0, Math.floor(now - at));
  if (s < 60) return s + 's';
  const m = Math.floor(s / 60);
  if (m < 60) return m + 'm';
  const h = Math.floor(m / 60);
  if (h < 48) return h + 'h';
  return Math.floor(h / 24) + 'd';
}
function opLabel(op) {
  if (!op) return 'idle';
  return String(op).replace(/^act:/, '');
}
function dateline() {
  const d = new Date();
  document.getElementById('dateline').textContent =
    d.toLocaleDateString('en-GB', { weekday: 'long', day: 'numeric', month: 'long' }) + ' · ' +
    d.toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit' });
}

function tabRow(tab, now) {
  const li = el('li', tab.current ? 'current' : '');
  li.appendChild(el('span', 'tid mono', tab.short));
  const middle = el('span');
  middle.appendChild(el('div', 'title', tab.title || tab.url || tab.short));
  middle.appendChild(el('div', 'host', tab.host || ''));
  li.appendChild(middle);
  li.appendChild(el('span', 'when mono', ago(tab.last_at, now)));
  li.dataset.url = tab.url || '';
  li.dataset.title = tab.title || '';
  li.addEventListener('click', () => switchTo(tab));
  return li;
}

async function switchTo(tab) {
  // The extension tab with this URL (then title): activate it and raise its window.
  try {
    const all = await chrome.tabs.query({});
    let hit = all.find((t) => t.url === tab.url) || all.find((t) => tab.title && t.title === tab.title);
    if (!hit) return;
    await chrome.tabs.update(hit.id, { active: true });
    await chrome.windows.update(hit.windowId, { focused: true });
  } catch (err) {
    console.warn('switch failed', err);
  }
}

function render() {
  const now = Date.now() / 1000;
  body.replaceChildren();
  const snap = snapshot;
  if (!snap || snap.version !== SNAPSHOT_VERSION) {
    document.body.classList.remove('plain');
    body.appendChild(el('p', 'standing', 'Waiting for herdr…'));
    return;
  }
  if (snap.show_activity === false) {
    document.body.classList.add('plain');
    return;
  }
  document.body.classList.remove('plain');
  const agents = Array.isArray(snap.agents) ? snap.agents : [];
  const yours = Array.isArray(snap.yours) ? snap.yours : [];
  const standing = el('p', 'standing');
  const n = agents.length;
  standing.appendChild(el('b', '', n + (n === 1 ? ' agent' : ' agents')));
  standing.appendChild(document.createTextNode(' working in '));
  const m = Number(snap.agent_tabs) || 0;
  standing.appendChild(el('b', '', m + (m === 1 ? ' tab' : ' tabs')));
  standing.appendChild(document.createTextNode(' · profile '));
  standing.appendChild(el('b', '', snap.profile || 'main'));
  if (snap.last_at) standing.appendChild(document.createTextNode(' · last action ' + ago(snap.last_at, now) + ' ago'));
  body.appendChild(standing);

  const title = el('section', 'agents-title');
  title.appendChild(el('h2', '', 'Agents'));
  body.appendChild(title);
  if (!agents.length) {
    body.appendChild(el('p', 'empty', 'No agents in the browser right now.'));
  }
  for (const agent of agents) {
    const section = el('section', agent.active ? 'agent live' : 'agent');
    const head = el('div', 'agent-head');
    head.appendChild(el('span', 'sym', agent.symbol || '◌'));
    head.appendChild(el('span', 'name', agent.label || agent.pane_id || ''));
    head.appendChild(el('span', 'where mono', agent.where || ''));
    const state = agent.last_at ? opLabel(agent.last_op) + ' · ' + ago(agent.last_at, now) : 'idle';
    head.appendChild(el('span', 'state mono', agent.active ? state : 'idle · ' + ago(agent.last_at, now)));
    section.appendChild(head);
    const list = el('ul', 'tabs');
    for (const tab of agent.tabs || []) list.appendChild(tabRow(tab, now));
    section.appendChild(list);
    body.appendChild(section);
  }
  if (yours.length) {
    const section = el('section', 'yours');
    section.appendChild(el('h2', '', 'Yours'));
    const list = el('ul', 'tabs');
    for (const tab of yours) list.appendChild(tabRow(tab, now));
    section.appendChild(list);
    body.appendChild(section);
  }
}

async function load() {
  try {
    const stored = await chrome.storage.session.get('snapshot');
    snapshot = stored && stored.snapshot ? stored.snapshot : null;
  } catch (err) {
    snapshot = null;
  }
  render();
}

chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== 'session' || !changes.snapshot) return;
  snapshot = changes.snapshot.newValue || null;
  render();
});
dateline();
setInterval(() => { dateline(); render(); }, 1000);
load();
