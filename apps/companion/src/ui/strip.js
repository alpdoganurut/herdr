// Strip + toolbar renderer. State comes from main (strip:state); actions go back via stripBridge.
'use strict';
const $ = (id) => document.getElementById(id);
const bridge = window.stripBridge;
let state = null;
let urlEditing = false;
// The dock's bar (`strip.html?mode=docked`): Dusk, no traffic-light inset, no drag region.
if (new URLSearchParams(location.search).get('mode') === 'docked') document.body.classList.add('docked');

const act = (name, args) => bridge.action(name, args);

const SVG = 'http://www.w3.org/2000/svg';
function icon(name, size) {
  const svg = document.createElementNS(SVG, 'svg');
  svg.setAttribute('class', size ? `ico ${size}` : 'ico');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS(SVG, 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.append(use);
  return svg;
}

function reportHeight() {
  act('chrome-height', Math.ceil($('chrome').getBoundingClientRect().height));
}

function render() {
  if (!state) return;
  document.body.classList.toggle('noinset', !state.inset);
  if (state.docked) document.body.classList.add('docked');
  const pill = $('count');
  const info = state.info;
  if (info && info.total > 0) {
    pill.hidden = false;
    pill.textContent = `${info.here} · ${info.total}`;
    const pages = (n) => `${n} page${n === 1 ? '' : 's'}`;
    pill.title = `${pages(info.here)} in this session · ${pages(info.total)} in ${info.sessions} session${info.sessions === 1 ? '' : 's'}\nClick for every session with pages`;
  } else pill.hidden = true;
  const sess = $('session');
  sess.textContent = '';
  if (state.session) {
    if (state.session.workspace) {
      const ws = document.createElement('span');
      ws.className = 'ws';
      ws.textContent = state.session.workspace;
      sess.append(ws);
    }
    sess.append(state.session.label);
  }
  $('session').title = state.session ? `herdr tab ${state.session.tab_id}` : 'no herdr tab';
  const tabs = $('tabs');
  tabs.textContent = '';
  for (const t of state.tabs) {
    const el = document.createElement('div');
    el.className = 'tab' + (t.active ? ' active' : '') + (t.loading ? ' loading' : '');
    el.setAttribute('role', 'tab');
    el.title = `${t.title || t.url}\n${t.url}${t.opened_by && t.opened_by !== 'user' ? `\nopened by ${t.opened_by}` : ''}`;
    if (t.needs_you) {
      const d = document.createElement('span');
      d.className = 'attn';
      d.title = 'an agent asked for you here';
      d.append(icon('attention', 'sm'));
      el.append(d);
    } else if (t.loading) {
      const sp = document.createElement('span');
      sp.className = 'spin';
      el.append(sp);
    }
    if (t.kind === 'html_view') {
      const k = document.createElement('span');
      k.className = 'kind';
      k.textContent = '◇';
      k.title = 'agent HTML view (sandboxed)';
      el.append(k);
    }
    const title = document.createElement('span');
    title.className = 'title';
    title.textContent = t.title || t.url || 'New tab';
    const close = document.createElement('button');
    close.className = 'close';
    close.append(icon('close', 'sm'));
    close.title = 'Close tab (⌘W)';
    close.addEventListener('click', (e) => {
      e.stopPropagation();
      act('close', t.view_id);
    });
    el.append(title, close);
    el.addEventListener('mousedown', (e) => {
      if (e.button === 1) {
        e.preventDefault();
        act('close', t.view_id);
      } else if (e.button === 0) act('select', t.view_id);
    });
    tabs.append(el);
  }
  const nav = state.nav || {};
  $('back').disabled = !nav.canGoBack;
  $('forward').disabled = !nav.canGoForward;
  $('reload').hidden = !!nav.loading;
  $('stop').hidden = !nav.loading;
  $('send').disabled = !state.attached || !state.active;
  $('newtab').disabled = !state.session;
  if (!urlEditing) $('url').value = state.active ? state.active.url : '';
  $('url').disabled = !state.session;
  const pb = $('permbar');
  if (state.prompt) {
    pb.hidden = false;
    pb.dataset.id = state.prompt.id;
    $('permtext').textContent = `${state.prompt.origin} wants to ${state.prompt.label}`;
  } else {
    pb.hidden = true;
    delete pb.dataset.id;
  }
  const st = $('status');
  if (state.status && state.status.text) {
    st.hidden = false;
    st.textContent = state.status.text;
    st.className = state.status.level || '';
  } else st.hidden = true;
  const up = $('update');
  up.hidden = !state.update;
  // disabled only while its restart request runs (main.js clears it when that fails)
  up.disabled = !!(state.update && state.update.restarting);
  if (state.update) up.title = `herdr installed companion ${state.update.version}; Restart applies it (your pages come back)`;
  $('empty').hidden = !!state.active;
  $('emptytext').textContent = state.session ? 'No pages in this session.' : 'Waiting for herdr…';
  reportHeight();
}

bridge.onState((s) => {
  state = s;
  render();
});
bridge.onFocusUrl(() => {
  $('url').focus();
  $('url').select();
});

$('url').addEventListener('focus', () => {
  urlEditing = true;
  $('url').select();
});
$('url').addEventListener('blur', () => {
  urlEditing = false;
  render();
});
$('urlform').addEventListener('submit', (e) => {
  e.preventDefault();
  const v = $('url').value.trim();
  if (!v) return;
  urlEditing = false;
  $('url').blur();
  act('navigate', v);
});
$('url').addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    urlEditing = false;
    $('url').blur();
  }
});
$('back').addEventListener('click', () => act('back'));
$('forward').addEventListener('click', () => act('forward'));
$('reload').addEventListener('click', () => act('reload'));
$('stop').addEventListener('click', () => act('stop'));
$('newtab').addEventListener('click', () => act('new_tab'));
$('session').addEventListener('dblclick', () => act('go_to_tab'));
$('send').addEventListener('click', () => {
  $('sendbar').hidden = false;
  $('note').value = '';
  $('note').focus();
  reportHeight();
});
$('sendcancel').addEventListener('click', () => {
  $('sendbar').hidden = true;
  reportHeight();
});
$('sendbar').addEventListener('submit', (e) => {
  e.preventDefault();
  const note = $('note').value;
  $('sendbar').hidden = true;
  reportHeight();
  act('send_page', { note });
});
$('note').addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    $('sendbar').hidden = true;
    reportHeight();
  }
});
$('update').addEventListener('click', () => act('restart_update'));
$('count').addEventListener('click', () => act('sessions_menu'));
$('permallow').addEventListener('click', () => act('permission', { id: $('permbar').dataset.id, allow: true }));
$('permdeny').addEventListener('click', () => act('permission', { id: $('permbar').dataset.id, allow: false }));
// Shortcuts (⌘T/W/L/R/[/]) are application-menu accelerators in main.js.
new ResizeObserver(reportHeight).observe($('chrome'));
act('ready');
