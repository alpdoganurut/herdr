// Strip + toolbar renderer. State comes from main (strip:state); actions go back via stripBridge.
'use strict';
const $ = (id) => document.getElementById(id);
const bridge = window.stripBridge;
const model = window.BarModel;
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

// The bar's laid-out height (also while hidden: the layout keeps it) and whether it shows.
let reported = null;
function reportHeight() {
  const height = Math.ceil($('chrome').getBoundingClientRect().height);
  const shown = !document.documentElement.classList.contains('bar-hidden');
  if (reported && reported.height === height && reported.shown === shown) return;
  reported = { height, shown };
  act('bar', reported);
}

// ---------- auto-hide (unpinned): hover, held, pulses ----------
const reveal = { hover: false, dwell: null, grace: null, focusing: false, pulseUntil: 0, pulseTimer: null, pulse: null };

function barHeld() {
  if (!state) return true;
  return model.held({
    urlFocus: urlEditing,
    menu: !!(state.bar && state.bar.menu),
    sendbar: !$('sendbar').hidden,
    permbar: !!state.prompt,
    status: !!(state.status && state.status.text),
    needsYou: (state.tabs || []).some((t) => t.needs_you),
    empty: !state.active,
  });
}

function barShown() {
  if (!state || !state.bar || state.bar.pinned) return true;
  return barHeld() || reveal.hover || reveal.focusing || Date.now() < reveal.pulseUntil;
}

function applyReveal() {
  const shown = barShown();
  const root = document.documentElement;
  root.classList.toggle('bar-hidden', !shown);
  root.classList.toggle('bar-pinned', !!(state && state.bar && state.bar.pinned));
  const hint = $('hint');
  hint.classList.toggle('loading', !!(state && state.nav && state.nav.loading));
  hint.classList.toggle('attn', !!(state && (state.tabs || []).some((t) => t.needs_you)));
  reportHeight();
}

// A pulse (main counts them): the bar shows for PULSE_MS; the first state only records it.
function notePulse() {
  const n = state && state.bar ? state.bar.pulse : null;
  if (n === null || n === undefined) return;
  if (reveal.pulse !== null && n !== reveal.pulse) {
    reveal.pulseUntil = Date.now() + model.PULSE_MS;
    clearTimeout(reveal.pulseTimer);
    reveal.pulseTimer = setTimeout(applyReveal, model.PULSE_MS + 20);
  }
  reveal.pulse = n;
}

// The pointer in the bar (or in the hit zone of the hidden one): it shows after the dwell;
// out of it, it hides after the grace. `y` null: the pointer left the bar's view.
function onPointer(y) {
  const shown = barShown();
  const bottom = shown ? $('chrome').getBoundingClientRect().bottom : model.ZONE_PX;
  const inside = y !== null && y >= 0 && y < bottom;
  if (inside) {
    clearTimeout(reveal.grace);
    reveal.grace = null;
    if (reveal.hover || reveal.dwell) return;
    if (shown) {
      reveal.hover = true; // already showing (a pulse): it stays while the pointer is in it
      return;
    }
    reveal.dwell = setTimeout(() => {
      reveal.dwell = null;
      reveal.hover = true;
      applyReveal();
    }, model.DWELL_MS);
  } else {
    clearTimeout(reveal.dwell);
    reveal.dwell = null;
    if (!reveal.hover || reveal.grace) return;
    reveal.grace = setTimeout(() => {
      reveal.grace = null;
      reveal.hover = false;
      applyReveal();
    }, model.HIDE_MS);
  }
}

function render() {
  if (!state) return;
  document.body.classList.toggle('noinset', !state.inset);
  if (state.docked) document.body.classList.add('docked');
  const pill = $('count');
  const info = state.info;
  if (info && info.total > 0) {
    pill.hidden = false;
    $('counttext').textContent = `${info.here} · ${info.total}`;
    const pages = (n) => `${n} page${n === 1 ? '' : 's'}`;
    const where = state.session ? `herdr tab ${state.session.label}` : 'this herdr tab';
    pill.title = `${pages(info.here)} in ${where} · ${pages(info.total)} in ${info.sessions} herdr tab${info.sessions === 1 ? '' : 's'}\nClick for the herdr tabs with pages`;
  } else pill.hidden = true;
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
    // Close acts on mousedown, like the tab's select: a select on the tab's mousedown re-renders
    // the strip, which replaced this button before its click could fire (the close was lost).
    close.addEventListener('mousedown', (e) => {
      e.stopPropagation();
      if (e.button !== 0) return;
      e.preventDefault();
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
  if (!urlEditing) {
    const url = state.active ? state.active.url : '';
    $('url').value = model.compactUrl(url);
    $('url').title = url;
  }
  $('url').disabled = !state.session;
  renderWindowActions();
  const pinned = !!(state.bar && state.bar.pinned);
  const pin = $('pin');
  pin.hidden = !state.bar;
  pin.setAttribute('aria-pressed', String(pinned));
  pin.title = pinned ? 'Unpin the bar: it hides until the pointer comes to the top (⌘⇧.)' : 'Pin the bar: it stays and the page starts below it (⌘⇧.)';
  pin.querySelector('use').setAttribute('href', pinned ? '#i-pinned' : '#i-pin');
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
  notePulse();
  applyReveal();
}

// The bar's window actions (docked only, and only when herdr advertises them): expand to
// herdr's Browser view (restore to the dock there) and close.
function renderWindowActions() {
  const actions = (state.bar && state.bar.actions) || { expand: null, close: false };
  const expand = $('expand');
  expand.hidden = !actions.expand;
  if (actions.expand) {
    const restore = actions.expand === 'restore';
    expand.dataset.action = actions.expand;
    expand.title = restore ? 'Back to the dock beside the terminal' : 'Expand to herdr\'s Browser view';
    expand.querySelector('use').setAttribute('href', restore ? '#i-restore' : '#i-expand');
  }
  const close = $('dockclose');
  close.hidden = !actions.close;
  close.title = actions.expand === 'restore' ? 'Close the Browser view (back to the terminal)' : 'Close the browser dock';
  $('win').hidden = [...$('win').querySelectorAll('button')].every((b) => b.hidden);
}

// One row, or two below the breakpoint.
function layoutRows() {
  const narrow = model.isNarrow(document.documentElement.clientWidth);
  if ($('bar').classList.contains('narrow') !== narrow) $('bar').classList.toggle('narrow', narrow);
}

bridge.onState((s) => {
  state = s;
  render();
});
// ⌘L: the hidden bar shows first (a field under visibility: hidden takes no focus); the
// focused field then holds it open until blur or Esc.
bridge.onFocusUrl(() => {
  reveal.focusing = true;
  applyReveal();
  $('url').focus();
  $('url').select();
  reveal.focusing = false;
  applyReveal();
});

$('url').addEventListener('focus', () => {
  urlEditing = true;
  applyReveal();
  $('bar').classList.add('editing');
  // the whole URL while editing
  if (state && state.active) $('url').value = state.active.url;
  $('url').select();
});
$('url').addEventListener('blur', () => {
  urlEditing = false;
  $('bar').classList.remove('editing');
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
$('expand').addEventListener('click', () => act('placement', $('expand').dataset.action));
$('pin').addEventListener('click', () => {
  if (!state || !state.bar) return;
  act('pin', { mode: state.bar.mode, pinned: !state.bar.pinned });
});
$('dockclose').addEventListener('click', () => act('placement', 'close'));
// ⌘⇧S clicks this too: the note bar holds the bar open, so it shows before the note takes
// the keyboard (a field under visibility: hidden takes no focus).
$('send').addEventListener('click', () => {
  $('sendbar').hidden = false;
  applyReveal();
  $('note').value = '';
  $('note').focus();
});
$('sendcancel').addEventListener('click', () => {
  $('sendbar').hidden = true;
  applyReveal();
});
$('sendbar').addEventListener('submit', (e) => {
  e.preventDefault();
  const note = $('note').value;
  $('sendbar').hidden = true;
  applyReveal();
  act('send_page', { note });
});
$('note').addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    $('sendbar').hidden = true;
    applyReveal();
  }
});
$('update').addEventListener('click', () => act('restart_update'));
$('count').addEventListener('click', () => act('sessions_menu'));
$('permallow').addEventListener('click', () => act('permission', { id: $('permbar').dataset.id, allow: true }));
$('permdeny').addEventListener('click', () => act('permission', { id: $('permbar').dataset.id, allow: false }));
// Shortcuts (⌘T/W/L/R/[/]) are application-menu accelerators in main.js.
new ResizeObserver(reportHeight).observe($('chrome'));
new ResizeObserver(layoutRows).observe(document.documentElement);
document.addEventListener('mousemove', (e) => onPointer(e.clientY));
document.documentElement.addEventListener('mouseleave', () => onPointer(null));
layoutRows();
act('ready');
