// Page API v1 (window.herdr.send): the main process's checks, pure so they can be tested
// without Electron. A page an agent showed with page_api gets window.herdr in its top frame
// (preload/page.js); every call is re-derived here from the IPC event (sender webContents,
// sender frame), never from what the page says, then re-checked by herdr
// (browser.page_message). A gesture is no proof of a human (CDP clicks are trusted input),
// so the rate limit is anti-spam only; herdr drops sends while an agent drives the page.
'use strict';

const TEXT_MAX = 4000; // characters
const DATA_MAX = 32 * 1024; // bytes of the data's JSON text
const FEATURE = 'page_messages'; // herdr advertises it while [browser] page_api is on
// Per view: a burst of 3, then one every 2 s, at most 30 an hour.
const RATE = { burst: 3, refillMs: 2000, hourMax: 30 };

// Whether a view entry was shown with the page API (and may keep it).
function optedIn(entry) {
  return !!(entry && entry.kind === 'html_view' && entry.page_api && !entry.allow_network && /^agent:./.test(entry.opened_by || '') && entry.file);
}

// The sender of a page-message IPC. `ctx`:
//   features    Set of herdr's lease features
//   viewId      the view whose webContents sent it (null: not a page view)
//   hit         {group, entry} of that view (null: unknown)
//   frame       e.senderFrame (null after its document went)
//   mainFrame   the sender webContents' main frame
//   shown       whether the view is the shown page of the shown session
//   token       the token the call carries; expected: the view's current one
//               (skip both for a hello)
//   realpath, fileURLToPath  injected (fs.realpathSync, url.fileURLToPath)
// -> {ok: true} or {ok: false, reason}
function checkSender(ctx, { hello = false } = {}) {
  const no = (reason) => ({ ok: false, reason });
  if (!ctx.features || !ctx.features.has(FEATURE)) return no('off');
  if (!ctx.viewId || !ctx.hit) return no('unknown_view');
  if (!optedIn(ctx.hit.entry)) return no('not_opted_in');
  const f = ctx.frame;
  if (!f) return no('no_frame');
  try {
    if ((typeof f.isDestroyed === 'function' && f.isDestroyed()) || f.detached) return no('frame_gone');
    if (f.parent) return no('subframe');
    if (ctx.mainFrame && f !== ctx.mainFrame) return no('subframe');
  } catch {
    return no('frame_gone');
  }
  let sent;
  let shown;
  try {
    sent = ctx.realpath(ctx.fileURLToPath(f.url));
    shown = ctx.realpath(ctx.hit.entry.file);
  } catch {
    return no('wrong_file');
  }
  if (!sent || sent !== shown) return no('wrong_file');
  if (hello) return { ok: true };
  if (!ctx.expected || ctx.token !== ctx.expected) return no('stale_document');
  if (!ctx.shown) return no('not_shown');
  return { ok: true };
}

// The page's payload, cut and checked again (the preload already did): text a string of at
// most TEXT_MAX characters, data_json JSON text of at most DATA_MAX bytes.
function checkPayload(req) {
  const no = (reason) => ({ ok: false, reason });
  if (!req || typeof req !== 'object') return no('invalid');
  const text = req.text === undefined || req.text === null ? '' : req.text;
  if (typeof text !== 'string') return no('invalid');
  if ([...text].length > TEXT_MAX) return no('too_large');
  let dataJson = null;
  if (req.data_json !== undefined && req.data_json !== null) {
    if (typeof req.data_json !== 'string') return no('invalid');
    if (Buffer.byteLength(req.data_json, 'utf8') > DATA_MAX) return no('too_large');
    try {
      JSON.parse(req.data_json);
    } catch {
      return no('invalid');
    }
    dataJson = req.data_json;
  }
  if (!text.trim() && dataJson === null) return no('empty');
  return { ok: true, text, data_json: dataJson };
}

// A token bucket with an hourly cap; `take(now)` spends one or answers false.
class RateLimit {
  constructor({ burst, refillMs, hourMax } = RATE, now = Date.now()) {
    this.burst = burst;
    this.refillMs = refillMs;
    this.hourMax = hourMax;
    this.tokens = burst;
    this.at = now;
    this.hour = []; // send times within the last hour
  }

  take(now = Date.now()) {
    this.tokens = Math.min(this.burst, this.tokens + (now - this.at) / this.refillMs);
    this.at = now;
    this.hour = this.hour.filter((t) => now - t < 3600 * 1000);
    if (this.tokens < 1 || this.hour.length >= this.hourMax) return false;
    this.tokens -= 1;
    this.hour.push(now);
    return true;
  }
}

module.exports = { TEXT_MAX, DATA_MAX, FEATURE, RATE, optedIn, checkSender, checkPayload, RateLimit };
