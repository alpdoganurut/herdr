// The herdr+ new tab page: renders the snapshot herdr pushes into the
// worker's session storage (`snapshot`), live through storage.onChanged.
// Every title, URL and label is untrusted page text: textContent only.
// The 1 s tick only refreshes ages and the live/idle state in place; the
// list is rebuilt when a new snapshot arrives.
'use strict';

const SNAPSHOT_VERSION = 1;
const DEFAULT_ACTIVE_SECS = 60;
const body = document.getElementById('body');
let snapshot = null;
// Live nodes the tick updates: [{ node, at }] for ages, [{ section, stateNode, agent }] for agents.
let ageNodes = [];
let agentNodes = [];
let lastActionNode = null;

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
function activeSecs(snap) {
  const n = Number(snap && snap.active_secs);
  return Number.isFinite(n) && n > 0 ? n : DEFAULT_ACTIVE_SECS;
}
function isLive(agent, now, window) {
  return !!agent.last_at && now - agent.last_at <= window;
}
function agentState(agent, live, now) {
  if (!agent.last_at) return 'idle';
  return live ? opLabel(agent.last_op) + ' · ' + ago(agent.last_at, now) : 'idle · ' + ago(agent.last_at, now);
}

function tabRow(tab, now, tabIds) {
  const li = el('li', tab.current ? 'current' : '');
  li.appendChild(el('span', 'tid mono', tab.short));
  const middle = el('span');
  middle.appendChild(el('div', 'title', tab.title || tab.url || tab.short));
  middle.appendChild(el('div', 'host', tab.host || ''));
  li.appendChild(middle);
  const when = el('span', 'when mono', ago(tab.last_at, now));
  li.appendChild(when);
  ageNodes.push({ node: when, at: tab.last_at });
  // The Chrome tab id herdr's sidecar resolved for this row: the click goes
  // to exactly that tab, nothing else (no URL or title lookup).
  const tabId = tabIds && tab.target != null ? tabIds[tab.target] : undefined;
  if (typeof tabId === 'number') {
    li.classList.add('switchable');
    li.addEventListener('click', () => switchTo(tabId));
  } else {
    li.classList.add('unresolved');
  }
  return li;
}

async function switchTo(tabId) {
  try {
    const hit = await chrome.tabs.get(tabId);
    await chrome.tabs.update(hit.id, { active: true });
    await chrome.windows.update(hit.windowId, { focused: true });
  } catch (err) {
    // The tab is gone (or the id is stale): show the list as it is now.
    render();
  }
}

function render() {
  const now = Date.now() / 1000;
  body.replaceChildren();
  ageNodes = [];
  agentNodes = [];
  lastActionNode = null;
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
  const window = activeSecs(snap);
  const tabIds = snap.tabIds && typeof snap.tabIds === 'object' ? snap.tabIds : null;
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
  if (snap.last_at) {
    lastActionNode = document.createTextNode(' · last action ' + ago(snap.last_at, now) + ' ago');
    standing.appendChild(lastActionNode);
  }
  body.appendChild(standing);

  const title = el('section', 'agents-title');
  title.appendChild(el('h2', '', 'Agents'));
  body.appendChild(title);
  if (!agents.length) {
    body.appendChild(el('p', 'empty', 'No agents in the browser right now.'));
  }
  for (const agent of agents) {
    const live = isLive(agent, now, window);
    const section = el('section', live ? 'agent live' : 'agent');
    const head = el('div', 'agent-head');
    head.appendChild(el('span', 'sym', agent.symbol || '◌'));
    head.appendChild(el('span', 'name', agent.label || agent.pane_id || ''));
    head.appendChild(el('span', 'where mono', agent.where || ''));
    const stateNode = el('span', 'state mono', agentState(agent, live, now));
    head.appendChild(stateNode);
    section.appendChild(head);
    agentNodes.push({ section, stateNode, agent });
    const list = el('ul', 'tabs');
    for (const tab of agent.tabs || []) list.appendChild(tabRow(tab, now, tabIds));
    section.appendChild(list);
    body.appendChild(section);
  }
  if (yours.length) {
    const section = el('section', 'yours');
    section.appendChild(el('h2', '', 'Yours'));
    const list = el('ul', 'tabs');
    for (const tab of yours) list.appendChild(tabRow(tab, now, tabIds));
    section.appendChild(list);
    body.appendChild(section);
  }
}

// Every second: ages and live/idle only; the DOM is not rebuilt.
function tick() {
  dateline();
  const snap = snapshot;
  if (!snap || snap.version !== SNAPSHOT_VERSION) return;
  const now = Date.now() / 1000;
  const window = activeSecs(snap);
  for (const { node, at } of ageNodes) node.textContent = ago(at, now);
  if (lastActionNode) lastActionNode.textContent = ' · last action ' + ago(snap.last_at, now) + ' ago';
  for (const { section, stateNode, agent } of agentNodes) {
    const live = isLive(agent, now, window);
    section.classList.toggle('live', live);
    stateNode.textContent = agentState(agent, live, now);
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
setInterval(tick, 1000);
load();
