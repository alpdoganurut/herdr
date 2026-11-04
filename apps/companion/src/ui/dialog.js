// The dialog card: shows the dialog main.js sends (or none), answers it. Enter = OK, Esc =
// Cancel (an alert's Esc is its OK). A prompt's field holds the default value, selected, so
// "Copy this:" prompts can be copied at once.
'use strict';
const $ = (id) => document.getElementById(id);
const bridge = window.dialogBridge;
let current = null;
let space = 0; // the height the card may take (main.js); a long message scrolls to fit it

function report() {
  const card = $('card');
  const h = card.hidden ? 0 : Math.ceil(card.getBoundingClientRect().height) + 14;
  bridge.size(h);
}

// The message gets what the rest of the card leaves of the space (at most 240px), so the
// field and the buttons stay on screen in a short window. Measured without the message, so
// its own cap never feeds back into it.
function fit() {
  const card = $('card');
  const msg = $('message');
  if (card.hidden || !space) return;
  const rest = card.getBoundingClientRect().height - msg.getBoundingClientRect().height + 14;
  msg.style.maxHeight = `${Math.max(24, Math.min(240, Math.floor(space - rest)))}px`;
}

function show(d) {
  current = d;
  if (d && d.space) space = d.space;
  const card = $('card');
  document.body.classList.toggle('docked', !!(d && d.docked));
  if (!d) {
    card.hidden = true;
    report();
    return;
  }
  card.hidden = false;
  card.dataset.type = d.type;
  $('origin').textContent = d.origin ? `${d.origin} says` : 'This page says';
  $('message').textContent = d.message;
  const field = $('field');
  field.hidden = d.type !== 'prompt';
  field.value = d.type === 'prompt' ? d.value : '';
  field.setSelectionRange(0, field.value.length); // selected even before the keyboard comes
  $('cancel').hidden = d.type === 'alert';
  fit();
  report();
  if (d.focus) focusCard();
}

function focusCard() {
  if (!current) return;
  if (current.type === 'prompt') {
    $('field').focus();
    $('field').select();
  } else $('ok').focus();
}

function answer(accept) {
  if (!current) return;
  const d = current;
  current = null;
  bridge.answer(d.id, accept, d.type === 'prompt' ? $('field').value : undefined);
}

$('card').addEventListener('submit', (e) => {
  e.preventDefault();
  answer(true);
});
$('cancel').addEventListener('click', () => answer(false));
$('field').addEventListener('input', () => current && bridge.input(current.id, $('field').value));
document.addEventListener('keydown', (e) => {
  if (!current) return;
  if (e.key === 'Escape') {
    e.preventDefault();
    answer(current.type === 'alert');
    return;
  }
  if (e.key === 'Enter' && !e.isComposing && document.activeElement !== $('cancel')) {
    e.preventDefault();
    answer(true);
    return;
  }
  // the edit keys without a menu bar (docked, the companion is an accessory app)
  const mod = navigator.platform.startsWith('Mac') ? e.metaKey : e.ctrlKey;
  if (mod && !e.altKey && !e.shiftKey) {
    const cmd = { c: 'copy', x: 'cut', v: 'paste', a: 'selectAll' }[e.key.toLowerCase()];
    if (cmd) {
      e.preventDefault();
      document.execCommand(cmd);
    }
  }
});
new ResizeObserver(report).observe($('card'));
bridge.onShow(show);
bridge.onFocus(focusCard);
bridge.onSpace((h) => {
  space = Number(h) || 0;
  fit();
});
bridge.ready();
