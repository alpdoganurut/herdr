// A page's JavaScript dialogs (alert, confirm, prompt): Electron has no prompt() and shows
// alert/confirm as native sheets, so every page view's preload (preload/page.js) replaces the
// three in the page's world with a synchronous IPC call and the companion shows them in its own
// modal (ui/dialog.html). This is the bookkeeping, without Electron (pure, unit-tested).
//
// Semantics (as a browser): the page is blocked until the dialog is answered; alert returns
// nothing, confirm a boolean, prompt the text or null (cancel). One dialog at a time per view:
// another one asked meanwhile (a cross-origin frame runs in its own process) waits behind it.
// A navigation of the frame that asked, a closed view or a quit cancels it (null / false).
'use strict';

const TYPES = new Set(['alert', 'confirm', 'prompt']);
const TEXT_MAX = 100000; // what the modal and herdr ever see of a message or a default value

// What the page called with, as the page's world coerced it (preload/page.js): strings already;
// anything else is treated as no text.
function normalizeRequest(req) {
  const r = req && typeof req === 'object' ? req : {};
  const type = TYPES.has(r.type) ? r.type : null;
  const text = (v) => (typeof v === 'string' ? v.slice(0, TEXT_MAX) : '');
  return type ? { type, message: text(r.message), default_value: type === 'prompt' ? text(r.default_value) : '' } : null;
}

// The value the page's sync IPC call returns: alert null (the page returns undefined), confirm
// a boolean, prompt the text or null. `text` absent = what the field holds (the default unless
// the user typed).
function replyFor(dialog, accept, text) {
  if (dialog.type === 'alert') return null;
  if (dialog.type === 'confirm') return !!accept;
  if (!accept) return null;
  if (typeof text === 'string') return text;
  return typeof dialog.value === 'string' ? dialog.value : dialog.default_value;
}

class DialogBook {
  // onChange(): the open dialogs changed (the modal and herdr's sidecar follow).
  constructor({ onChange = () => {} } = {}) {
    this.onChange = onChange;
    this.queues = new Map(); // view_id -> [dialog] (the first one is shown)
    this.nextId = 1;
    this.seq = 0;
  }

  // A page asked. `reply(value)` answers its blocked call exactly once. `frame`: who asked
  // (its navigation cancels it); `info`: shown with it (origin, embedded). Returns the dialog,
  // or null for a request that is not a dialog (answered at once).
  open(viewId, req, reply, { frame = null, info = {} } = {}) {
    const n = normalizeRequest(req);
    if (!n || !viewId) {
      safeReply(reply, n && n.type === 'confirm' ? false : null);
      return null;
    }
    const dialog = { ...info, id: `d${this.nextId++}`, view_id: viewId, ...n, value: n.default_value, frame, reply, opened_at: Date.now() };
    const q = this.queues.get(viewId) || [];
    q.push(dialog);
    this.queues.set(viewId, q);
    this.changed();
    return dialog;
  }

  // The dialog the view shows (the first of its queue), or null.
  top(viewId) {
    const q = this.queues.get(viewId);
    return q && q.length ? q[0] : null;
  }

  has(viewId) {
    return !!this.top(viewId);
  }

  // The view whose shown dialog is `id`, or null.
  viewOf(id) {
    for (const [viewId, q] of this.queues) if (q[0].id === id) return viewId;
    return null;
  }

  // The user typed into the shown prompt: an accept without text returns it.
  setValue(viewId, id, value) {
    const d = this.top(viewId);
    if (!d || d.id !== id || typeof value !== 'string') return false;
    d.value = value.slice(0, TEXT_MAX);
    return true;
  }

  // Answers the view's shown dialog (`id`, when given, must be it): accept = OK, else Cancel.
  // Returns the answered dialog, or null when there is none (or another one is shown now).
  answer(viewId, { id = null, accept = false, text } = {}) {
    const d = this.top(viewId);
    if (!d || (id && d.id !== id)) return null;
    this.queues.get(viewId).shift();
    if (!this.queues.get(viewId).length) this.queues.delete(viewId);
    safeReply(d.reply, replyFor(d, accept, text));
    this.changed();
    return d;
  }

  // Cancels the view's dialogs (all of them, or only those `frame` asked): a navigation, a
  // closed view. Returns how many.
  dismissView(viewId, { frame = null } = {}) {
    const q = this.queues.get(viewId);
    if (!q) return 0;
    const gone = frame == null ? q : q.filter((d) => d.frame === frame);
    if (!gone.length) return 0;
    const keep = frame == null ? [] : q.filter((d) => d.frame !== frame);
    if (keep.length) this.queues.set(viewId, keep);
    else this.queues.delete(viewId);
    for (const d of gone) safeReply(d.reply, replyFor(d, false));
    this.changed();
    return gone.length;
  }

  // Every view's (the companion quits).
  dismissAll() {
    let n = 0;
    for (const viewId of [...this.queues.keys()]) n += this.dismissView(viewId);
    return n;
  }

  // The shown dialog of every view, for herdr (`target_of(view_id)`: its CDP target id).
  list(targetOf = () => null) {
    const out = [];
    for (const [viewId, q] of this.queues) {
      const d = q[0];
      out.push({ id: d.id, view_id: viewId, target_id: targetOf(viewId) || null, type: d.type, message: d.message, default_value: d.default_value, queued: q.length - 1 });
    }
    return out;
  }

  changed() {
    this.seq++;
    try {
      this.onChange();
    } catch {}
  }
}

function safeReply(reply, value) {
  try {
    if (reply) reply(value);
  } catch {}
}

module.exports = { DialogBook, normalizeRequest, replyFor, TEXT_MAX };
