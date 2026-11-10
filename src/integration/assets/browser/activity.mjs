// The activity overlay: the glow frame and the cursor injected into a page
// (an isolated world, a closed shadow root, no pointer events) while an agent
// works on it, and the companion extension's per-pane tab groups, driven over
// CDP. The look is the approved one from the overlay spike; its values are
// reused verbatim.

export const WORLD = 'herdr';
export const HOST_ATTR = 'data-herdr-overlay';
/** The companion worker code this sidecar expects (VERSION in companion/sw.js); an older running worker is reloaded. */
export const COMPANION_VERSION = 11;
/** The activity window when the directive names none (`linger_ms`, [browser] active_glyph_secs). */
export const LINGER_MS = 120000;
/** The frame fades this long after the agent's last act (or at the end of the window when that is shorter): steady through a burst of acts 5–20 s apart, gone soon after. */
export const FRAME_LINGER_MS = 15000;
/** The cursor hides this long after the last act that moved it (or at the end of the window when that is shorter); the frame stays. */
export const CURSOR_LINGER_MS = 5000;
/** The cursor's glide (matches the CSS transition). */
export const GLIDE_MS = 350;
/** The longest window a timer takes (Node's setTimeout limit; a longer delay would fire at once). */
export const MAX_TIMER_MS = 2 ** 31 - 1;
/** One overlay evaluate may take at most this long; the op never waits longer. */
const EVAL_TIMEOUT_MS = 1500;
const COMPANION_CALL_MS = 3000;
/** The blank tab that wakes an idled companion worker; never tracked. */
export const NUDGE_URL = 'about:blank#herdr-nudge';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function withTimeout(promise, ms, what) {
  let timer;
  const timeout = new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${what} took more than ${ms} ms`)), ms); });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/** The approved purple; `[browser] activity_color` overrides it per directive. */
export const DEFAULT_COLOR = '#aa6eff';
export function normalizeColor(value) {
  const text = String(value || '').trim().toLowerCase();
  return /^#[0-9a-f]{6}$/.test(text) ? text : DEFAULT_COLOR;
}

// Installed in the isolated world: `(OVERLAY_JS)(cursor, color, busy)`; idempotent
// per document, re-installed when the colour changed. `busy` is the look: the
// pulse while an operation runs, the calmer steady glow for the rest of the
// activity window. The alpha values are the approved look; only the rgb comes
// from the colour.
export const OVERLAY_JS = `((cursor, color, busy) => {
  const rgb = [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16)).join(',');
  const live = window.__herdrOverlay;
  if (live && live.alive && live.color === color) { live.show(); live.busy(busy); return 'present'; }
  if (live && live.alive) live.alive = false; // (the cursor comes from the caller: a hidden one stays hidden across a re-install)
  document.querySelectorAll('div[${HOST_ATTR}]').forEach((n) => n.remove());
  const host = document.createElement('div');
  host.setAttribute('${HOST_ATTR}', '');
  host.setAttribute('data-herdr-color', color);
  host.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;opacity:0;transition:opacity .25s';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = \`<style>
    .frame{position:fixed;inset:0;border:2px solid rgba(\${rgb},.9);box-shadow:inset 0 0 14px 3px rgba(\${rgb},.42);
      border-radius:6px;animation:pulse 2s ease-in-out infinite;transition:box-shadow .6s,border-color .6s}
    .frame.idle{animation:none;border-color:rgba(\${rgb},.7);box-shadow:inset 0 0 10px 2px rgba(\${rgb},.28)}
    @keyframes pulse{50%{box-shadow:inset 0 0 22px 6px rgba(\${rgb},.62)}}
    .cur{position:fixed;left:-5px;top:-2.5px;width:24px;height:24px;transition:transform .35s cubic-bezier(.2,.8,.2,1),opacity .4s;
      filter:drop-shadow(0 1px 2px rgba(0,0,0,.5))}
    .ripple{position:fixed;width:28px;height:28px;margin:-14px 0 0 -14px;border-radius:50%;border:2px solid rgba(\${rgb},.9);
      animation:rip .5s ease-out forwards}
    @keyframes rip{from{transform:scale(.3);opacity:1}to{transform:scale(1.6);opacity:0}}
  </style><div class="frame\${busy ? '' : ' idle'}"></div>
  <svg class="cur" viewBox="0 0 24 24"><path d="M5 2.5v17.2l4.3-4.2 2.9 6.6 2.6-1.1-2.9-6.5h6.1z" fill="\${color}" stroke="#fff" stroke-width="1.4" stroke-linejoin="round"/></svg>\`;
  (document.documentElement || document).appendChild(host);
  const frame = root.querySelector('.frame');
  const cur = root.querySelector('.cur');
  if (cursor) {
    cur.style.transition = 'none';
    cur.style.transform = 'translate(' + cursor.x + 'px,' + cursor.y + 'px)';
    void cur.offsetWidth;
    cur.style.transition = '';
  } else {
    cur.style.opacity = '0';
  }
  requestAnimationFrame(() => { host.style.opacity = '1'; });
  let hideTimer = null;
  const api = {
    alive: true,
    color,
    cursor: cursor || null,
    show() { if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; } host.style.transition = 'opacity .25s'; host.style.opacity = '1'; },
    busy(on) { frame.classList.toggle('idle', !on); },
    hide(fast) {
      // the window's end is a slow fade; a gone pane or a dismissed frame goes at once
      host.style.transition = fast ? 'opacity .25s' : 'opacity 1.2s';
      host.style.opacity = '0';
      hideTimer = setTimeout(() => { api.alive = false; host.remove(); if (window.__herdrOverlay === api) delete window.__herdrOverlay; }, fast ? 300 : 1300);
    },
    suspend(on) { host.style.visibility = on ? 'hidden' : ''; },
    moveTo(x, y, animate) {
      api.cursor = { x, y };
      cur.style.opacity = '1';
      if (!animate) cur.style.transition = 'none';
      cur.style.transform = 'translate(' + x + 'px,' + y + 'px)';
      if (!animate) { void cur.offsetWidth; cur.style.transition = ''; }
    },
    hideCursor() { api.cursor = null; cur.style.opacity = '0'; },
    probe() { return { frame: host.style.opacity, idle: frame.classList.contains('idle'), cursor: cur.style.opacity, hidden: host.style.visibility === 'hidden' }; },
    click(x, y) {
      const r = document.createElement('div');
      r.className = 'ripple'; r.style.left = x + 'px'; r.style.top = y + 'px';
      root.appendChild(r); setTimeout(() => r.remove(), 600);
    },
  };
  window.__herdrOverlay = api;
  return 'installed';
})`;

/** The overlay of one page: an isolated-world context per document, the
 *  cursor's last position, the two timers. The frame pulses while an op runs
 *  (`show`), settles to the calmer look when it is done (`linger`) and fades
 *  FRAME_LINGER_MS after the last act — every act re-shows it and restarts
 *  that — or at the end of the activity window (`linger_ms`,
 *  `[browser] active_glyph_secs`; the sidebar's ◎ and the group's ● keep the
 *  whole window) when that is shorter, or at once when the pane is gone
 *  (`dismiss`). The cursor hides on its own CURSOR_LINGER_MS after the last
 *  act that moved it (the window again the cap); a new document inside the
 *  window gets the frame back in the same state, the cursor only while it is
 *  still due (`reapply`). */
export class Overlay {
  constructor(state) {
    this.state = state;
    this.ctx = null;
    this.cursor = null;
    this.color = DEFAULT_COLOR;
    this.hideTimer = null;
    this.until = 0;
    this.active = false; // an op is running on the page
    this.cursorTimer = null;
    this.cursorUntil = 0; // Infinity between a move and the op's end
    this.cursorAt = 0; // when the last act moved the cursor
    this.frameLingerMs = FRAME_LINGER_MS; // (per instance so a check can shorten them)
    this.cursorLingerMs = CURSOR_LINGER_MS;
    this.log = () => {};
  }
  async context() {
    if (this.ctx != null) return this.ctx;
    const session = this.state.session;
    const tree = await session.send('Page.getFrameTree');
    const frameId = tree.frameTree.frame.id;
    const { executionContextId } = await session.send('Page.createIsolatedWorld', { frameId, worldName: WORLD, grantUniveralAccess: false });
    this.ctx = executionContextId;
    return this.ctx;
  }
  async eval(expression) {
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const contextId = await this.context();
        const r = await withTimeout(this.state.session.send('Runtime.evaluate', { expression, contextId, returnByValue: true }), EVAL_TIMEOUT_MS, 'overlay');
        if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception && r.exceptionDetails.exception.description || r.exceptionDetails.text || 'overlay script failed');
        return r.result ? r.result.value : undefined;
      } catch (err) {
        this.ctx = null;
        if (attempt === 1 || !/context|Cannot find|detached|destroyed|navigat/i.test(String(err.message))) throw err;
      }
    }
    return undefined;
  }
  /** Put the frame into the current document (idempotent; re-installed on a colour change), in the `busy` look or the idle one, the cursor where it was while it is still due. */
  install(busy) {
    const cursor = this.cursorUntil > Date.now() ? this.cursor : null;
    return this.eval(`(${OVERLAY_JS})(${JSON.stringify(cursor)}, ${JSON.stringify(this.color)}, ${busy ? 'true' : 'false'})`);
  }
  /** The activity window of a directive: as given, the default when missing or not a number, never past the timer's limit. */
  static window(ms) {
    const n = Number(ms);
    return Math.min(Number.isFinite(n) && n >= 0 ? n : LINGER_MS, MAX_TIMER_MS);
  }
  /** An op began: the frame is up and pulsing until `linger`. */
  async show(color) {
    if (color) this.color = normalizeColor(color);
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    this.active = true;
    this.until = Infinity;
    return this.install(true);
  }
  /** The op is done: the calmer look now; the frame fades FRAME_LINGER_MS from
   *  now and the cursor CURSOR_LINGER_MS from the act that moved it, each no
   *  later than the end of the window (`ms`, the directive's `linger_ms`; an
   *  explicit 0 means no linger at all). */
  linger(ms) {
    const window = Overlay.window(ms);
    const now = Date.now();
    const frame = Math.min(window, this.frameLingerMs);
    this.active = false;
    this.until = now + frame;
    this.eval('window.__herdrOverlay ? (__herdrOverlay.busy(false), "idle") : "absent"').catch(() => {});
    if (this.hideTimer) clearTimeout(this.hideTimer);
    this.hideTimer = setTimeout(() => {
      this.hideTimer = null;
      this.until = 0;
      this.eval('window.__herdrOverlay ? (__herdrOverlay.hide(false), "hidden") : "absent"').catch(() => {});
    }, frame);
    if (this.hideTimer.unref) this.hideTimer.unref();
    // the cursor: scheduled once per move, from the move; an op that did not move it leaves the timer be
    if (this.cursorUntil === Infinity) {
      const due = Math.max(0, this.cursorAt + Math.min(window, this.cursorLingerMs) - now);
      this.cursorUntil = Math.min(now + due, this.until);
      if (this.cursorTimer) clearTimeout(this.cursorTimer);
      this.cursorTimer = setTimeout(() => {
        this.cursorTimer = null;
        this.cursorUntil = 0;
        this.eval('window.__herdrOverlay ? (__herdrOverlay.hideCursor(), "cursor hidden") : "absent"').catch(() => {});
      }, Math.min(due, frame));
      if (this.cursorTimer.unref) this.cursorTimer.unref();
    }
  }
  /** The pane is gone: the frame and the cursor go now, whatever is left of the window. */
  dismiss() {
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    if (this.cursorTimer) { clearTimeout(this.cursorTimer); this.cursorTimer = null; }
    const up = this.until > Date.now();
    this.active = false;
    this.until = 0;
    this.cursorUntil = 0;
    if (!up) return Promise.resolve();
    return this.eval('window.__herdrOverlay ? (__herdrOverlay.hide(true), "hidden") : "absent"').catch(() => {});
  }
  /** Glide the cursor to a viewport point (and wait for the glide when animating); it shows until CURSOR_LINGER_MS after the op ends. */
  async moveTo(x, y, animate) {
    this.cursor = { x, y };
    this.cursorAt = Date.now();
    this.cursorUntil = Infinity;
    if (this.cursorTimer) { clearTimeout(this.cursorTimer); this.cursorTimer = null; }
    await this.eval(`__herdrOverlay.moveTo(${x},${y},${animate ? 'true' : 'false'})`);
    if (animate) await sleep(GLIDE_MS);
  }
  ripple(x, y) {
    return this.eval(`__herdrOverlay.click(${x},${y})`).catch(() => {});
  }
  /** Hidden while a screenshot is taken (the agent's picture is the page, not the overlay). */
  suspend(on) {
    if (this.until <= Date.now()) return Promise.resolve();
    return this.eval(`window.__herdrOverlay && __herdrOverlay.suspend(${on ? 'true' : 'false'})`).catch(() => {});
  }
  /** The main frame navigated: the world is gone with the document. */
  navigated() { this.ctx = null; }
  /** A new document while the frame should be up: put it back in the same state (the window is not stretched). */
  async reapply() {
    if (this.until > Date.now()) await this.install(this.active).catch((err) => this.log(`overlay reapply: ${err.message}`));
  }
  dispose() {
    if (this.hideTimer) { clearTimeout(this.hideTimer); this.hideTimer = null; }
    if (this.cursorTimer) { clearTimeout(this.cursorTimer); this.cursorTimer = null; }
    this.active = false;
    this.until = 0;
    this.cursorUntil = 0;
  }
}

/** The quiet select's share of a screenshot's deadline (the companion call is capped at 3 s). */
export const SELECT_BUDGET_MS = 3000;
/** The least a capture is given; below that the op fails rather than waits. */
const CAPTURE_MIN_MS = 250;

/** A screenshot capture that retries once after a quiet select when the tab
 *  did not paint (a background tab whose renderer stalled): `capture(ms)`
 *  runs with its paint timeout, a timeout asks `select()` (the companion's
 *  quiet tab selection, true when it happened) and runs `capture(ms)` once
 *  more; what fails after that — or could not be selected — is
 *  `tab_not_rendered` without any advice to bring the window forward. The
 *  whole thing stays inside `deadlineAt` (the op's deadline): each capture
 *  gets at most `stallMs` or what is left, and the select plus the second
 *  capture are skipped when what is left cannot cover them. A `scoped`
 *  capture (an element by ref or selector) is never retried: its timeout is
 *  the element's, not the tab's, and the original error goes back. Other
 *  errors pass through. */
export async function captureWithQuietRetry({ capture, select, log = () => {}, deadlineAt = Infinity, stallMs = 5000, scoped = false }) {
  const timedOut = (err) => /Timeout/i.test(String(err && err.message));
  const notRendered = () => Object.assign(new Error('the tab could not be drawn; try again'), { code: 'tab_not_rendered' });
  const budget = () => Math.min(stallMs, Math.max(CAPTURE_MIN_MS, deadlineAt - Date.now()));
  try {
    return await capture(budget());
  } catch (err) {
    if (!timedOut(err) || scoped) throw err;
    if (deadlineAt - Date.now() < SELECT_BUDGET_MS + CAPTURE_MIN_MS) {
      log('screenshot: the tab did not paint and the deadline leaves no room for a quiet select');
      throw notRendered();
    }
    const selected = await Promise.resolve().then(select).catch((e) => { log(`quiet select: ${e.message}`); return false; });
    if (!selected) throw notRendered();
    log('screenshot: the tab did not paint; selected it quietly, one more try');
    try {
      return await capture(budget());
    } catch (again) {
      if (timedOut(again)) throw notRendered();
      throw again;
    }
  }
}

/** How long a capture waits behind an earlier one that has not settled; after
 *  that it starts anyway (one Chromium never answers must not wedge a tab's
 *  screenshots for good). */
export const CAPTURE_STALE_MS = 60000;

/** One page's `Page.captureScreenshot` requests, one at a time. With a clip
 *  (full page, an element, a scaled copy) Chromium resizes the page's view for
 *  the capture and, when the capture settles (also when it fails), puts back
 *  the size the view had when the request arrived. A request that arrives
 *  while an earlier one is still pending reads the stretched size as the
 *  original and puts that back: the tab is left as tall as its document
 *  (innerHeight == scrollHeight) and cannot scroll. A screenshot that timed
 *  out on our side is still pending in Chromium, so every capture of a page
 *  queues here behind every earlier one until that one settles. */
export class CaptureLane {
  constructor({ staleMs = CAPTURE_STALE_MS } = {}) {
    this.tail = Promise.resolve();
    this.staleMs = staleMs;
  }
  /** `send()` runs once the requests queued before it have settled; when
   *  `wanted()` is false by then (the op gave up), it does not run at all. */
  enqueue(send, wanted = () => true) {
    let timer = null;
    const stale = new Promise((resolve) => { timer = setTimeout(resolve, this.staleMs); });
    const run = Promise.race([this.tail, stale])
      .finally(() => clearTimeout(timer))
      .then(() => (wanted() ? send() : Promise.reject(new Error('capture abandoned before it started'))));
    this.tail = run.then(() => {}, () => {});
    return run;
  }
}

/** A promise that rejects with a Timeout error after `ms` (the request itself goes on). */
export function captureTimeout(promise, ms) {
  let timer = null;
  const expired = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`Timeout ${ms}ms exceeded: the tab did not draw the capture`)), ms);
  });
  return Promise.race([promise, expired]).finally(() => clearTimeout(timer));
}

/** One screenshot through a page's lane, for captureWithQuietRetry: the first
 *  `wait(ms)` queues the request, a later one (the retry after a quiet
 *  select) waits on the same request rather than sending a second one beside
 *  it. `abandon()` (the op is over) keeps a request that has not started yet
 *  from starting; one Chromium already has is left to settle in the lane.
 *  `deadlineAt` (the op's, epoch ms): a request not sent by then never is. */
export function laneShot(lane, send, deadlineAt = null) {
  let request = null;
  let abandoned = false;
  let sent = false;
  let settled = false;
  // past the op's deadline its caller has been answered already: the request
  // is never sent, however long it waited in the lane
  const wanted = () => !abandoned && (deadlineAt == null || Date.now() < deadlineAt);
  const run = () => {
    sent = true;
    return Promise.resolve().then(send).finally(() => { settled = true; });
  };
  return {
    wait(ms) {
      if (!request) {
        if (!wanted()) return Promise.reject(new Error('capture abandoned before it started'));
        request = lane.enqueue(run, wanted);
        request.catch(() => {});
      }
      return captureTimeout(request, ms);
    },
    abandon() { abandoned = true; },
    /** Chromium has the request and has not answered it. */
    pending() { return sent && !settled; },
  };
}

/** How long a clipped capture waits for its tab to be ready: a size, its
 *  document loaded (a quiet select halfway when it has no size). */
export const CLIP_READY_MS = 4000;
/** A view the companion created, before its target id is known (its about:blank
 *  marker; the page itself loads after it). Defined here, before CLIP_PROBE uses it. */
export const VIEW_MARKER = 'about:blank#herdr-view=';
const CLIP_PROBE = `({ width: innerWidth, height: innerHeight, loaded: document.readyState === 'complete' && !location.href.startsWith(${JSON.stringify(VIEW_MARKER)}) })`;

/** Chromium's clipped Page.captureScreenshot (a full page, an element, the
 *  scaled inline copy) sets the clip as device-metrics emulation on the
 *  session (a viewport offset and scale) and takes it back when the capture
 *  settles, but only when the view had a size at request time, and never
 *  for a request a new document overtook (a navigation, a reload, the
 *  companion's about:blank marker giving way to the page right after a
 *  show: Chromium does not answer it at all). The emulation that stays makes
 *  the tab draw every frame translated by its scroll offset, through reloads,
 *  while the page itself and every capture look right. So a clipped capture
 *  first waits here for its tab to have a size and its document to be
 *  loaded, inside `waitMs` and the op's deadline: without a size by halfway
 *  it selects the tab quietly once (a companion view that was never shown
 *  has none; skipped when the deadline leaves no room for it, as in
 *  captureWithQuietRetry), still none at the end is `tab_not_rendered` and no capture is
 *  sent; a document still loading by then (a page that never finishes, a
 *  blank tab still on its marker) goes ahead. Answers the last probe. */
export async function clipReady({ send, select = null, deadlineAt = Infinity, log = () => {}, waitMs = CLIP_READY_MS, pollMs = 100 }) {
  const sized = (p) => Boolean(p && p.width > 0 && p.height > 0);
  const probe = () => captureTimeout(send('Runtime.evaluate', { expression: CLIP_PROBE, returnByValue: true }), 1000)
    .then((r) => (r && r.result ? r.result.value : null), () => null);
  const start = Date.now();
  const until = Math.min(start + waitMs, deadlineAt - CAPTURE_MIN_MS);
  let selected = false;
  let last = null;
  for (;;) {
    last = await probe();
    if (sized(last) && last.loaded) return last;
    if (Date.now() >= until) break;
    if (!selected && select && last && !sized(last) && Date.now() - start >= waitMs / 2) {
      selected = true;
      // as captureWithQuietRetry: no select when what is left cannot cover it and a capture
      if (deadlineAt - Date.now() < SELECT_BUDGET_MS + CAPTURE_MIN_MS) {
        log('screenshot: the tab has no size yet and the deadline leaves no room for a quiet select');
        continue;
      }
      const ok = await Promise.resolve().then(select).catch(() => false);
      log(`screenshot: the tab has no size yet; ${ok ? 'selected it quietly' : 'it could not be selected'}`);
      continue;
    }
    await new Promise((r) => setTimeout(r, pollMs));
  }
  if (last && !sized(last)) {
    throw Object.assign(new Error('the tab has not been shown yet, so it has no size to capture; show it (browser_focus) and try again'), { code: 'tab_not_rendered' });
  }
  if (last) log('screenshot: the page is still loading; capturing anyway');
  return last;
}

/** Takes back the device metrics a clipped capture left on the host's
 *  session (see clipReady). A no-op when there are none: the host never sets
 *  device metrics of its own on that session. */
export function clearClipEmulation(send, log = () => {}) {
  // the renderer answers it (a busy one may not soon): never more than 1 s
  return captureTimeout(Promise.resolve().then(() => send('Emulation.clearDeviceMetricsOverride')), 1000).then(() => true, () => false)
    .then((ok) => {
      if (!ok) log('screenshot: device metrics clear not confirmed within 1 s');
      return ok;
    });
}

/** A capture's request for the lane: the capture, then the device metrics a
 *  clipped one may have left cleared before the next capture of the page
 *  starts (after a viewport shot too: that heals a tab an older sidecar left
 *  offset on this session). */
export function cleanCapture(send, params, log = () => {}) {
  return async () => {
    try {
      return await send('Page.captureScreenshot', params);
    } finally {
      await clearClipEmulation(send, log);
    }
  };
}

/** The `Page.captureScreenshot` parameters for one shot. A viewport shot sends
 *  no clip, and Chromium then leaves the view's size alone; a clip (`beyond`
 *  for a full page or an element) resizes the view for the capture. */
export function shotParams({ type, quality, clip = null, beyond = false }) {
  const params = { format: type };
  if (type === 'jpeg') params.quality = quality;
  if (clip) params.clip = { x: clip.x, y: clip.y, width: clip.width, height: clip.height, scale: clip.scale || 1 };
  if (clip && beyond) params.captureBeyondViewport = true;
  return params;
}

/** A screenshot op's first capture, through the page's lane: the element's box
 *  (`element()`, measured first), the whole document (`full`, measured with
 *  Page.getLayoutMetrics) or the viewport (neither: no clip), then
 *  captureWithQuietRetry. A clipped one waits for clipReady first, has its
 *  device metrics cleared once Chromium answers (cleanCapture), and when
 *  the op gives up on it while Chromium still holds it (a request a new
 *  document overtook is never answered) clears them then. `send` is the
 *  host's CDP session, `select` the quiet select; every laneShot made goes
 *  into `shots` (the op abandons them when it is over). Answers Chromium's
 *  `{ data }`. */
export async function firstShot({ send, lane, type, quality, element = null, full = false, select, log = () => {}, deadlineAt, stallMs, shots }) {
  if (element || full) await clipReady({ send, select, deadlineAt, log });
  let clip = element ? await element() : null;
  if (!clip && full) {
    const metrics = await send('Page.getLayoutMetrics');
    const content = metrics.cssContentSize || metrics.contentSize;
    clip = { x: 0, y: 0, width: content.width, height: content.height };
  }
  const params = shotParams({ type, quality, clip, beyond: Boolean(clip) });
  const shot = laneShot(lane, cleanCapture(send, params, log), deadlineAt);
  shots.push(shot);
  // A background tab whose renderer stalled: select it quietly and wait
  // once more for the same capture, inside the op's deadline; an element
  // capture is not retried.
  try {
    return await captureWithQuietRetry({
      capture: (timeout) => shot.wait(timeout),
      select,
      log,
      deadlineAt,
      stallMs,
      scoped: Boolean(element),
    });
  } catch (err) {
    if (clip && shot.pending()) {
      log('screenshot: gave up on a clipped capture Chromium still holds; clearing its device metrics');
      await clearClipEmulation(send, log);
    }
    throw err;
  }
}

/** Panes that are gone: the overlay of every page a pane's directive touched
 *  (`state.paneKey`, recorded by the host when the directive arrived) goes
 *  now — whatever the companion's state, grouped or not. Answers how many. */
export function dismissPanes(profiles, keys) {
  const wanted = new Set(keys.map(String));
  let dismissed = 0;
  for (const profile of profiles) {
    if (!profile.pages) continue;
    for (const state of profile.pages.values()) {
      if (!state.closed && state.paneKey && wanted.has(state.paneKey) && state.overlay) {
        state.overlay.dismiss();
        dismissed++;
      }
    }
  }
  return dismissed;
}

/** Evaluated in the companion worker before a connect: every tab Chrome
 *  restored but never loaded (`status: 'unloaded'`) is reloaded. Such a tab
 *  never answers DevTools and hangs `connectOverCDP` (measured; a discard
 *  does not help — a discarded never-loaded tab still blocks). Only
 *  `chrome.tabs`, so an older worker runs it too. */
export const SWEEP_EXPR = `(async () => {
  const unloaded = await chrome.tabs.query({ status: 'unloaded' });
  const done = await Promise.allSettled(unloaded.map((t) => chrome.tabs.reload(t.id)));
  const left = unloaded.filter((t, i) => done[i].status !== 'fulfilled');
  // a nudge tab a cut-short rawNudge left behind (match patterns cannot name an about: URL with a fragment)
  const nudges = (await chrome.tabs.query({})).filter((t) => t.url === '${NUDGE_URL}');
  await Promise.allSettled(nudges.map((t) => chrome.tabs.remove(t.id)));
  return { unloaded: unloaded.length, reloaded: unloaded.length - left.length,
           left: left.slice(0, 3).map((t) => t.title || t.url), left_count: left.length, nudges_removed: nudges.length };
})()`;
/** Tabs still loading (the reloaded ones settle before the connect; a tab mid-load stalls it briefly). */
export const LOADING_EXPR = `chrome.tabs.query({ status: 'loading' }).then((ts) => ({ loading: ts.length, loading_titles: ts.slice(0, 3).map((t) => t.title || t.url) }))`;
/** The longest the sweep waits for the companion worker to be listed after a launch (measured ~1.2 s). */
export const SWEEP_WORKER_WAIT_MS = 3000;
/** The longest the sweep waits for reloaded tabs to finish loading. */
export const SWEEP_SETTLE_MS = 5000;

const titles = (list) => list.map((t) => `"${String(t).slice(0, 60)}"`).join(', ');

/** One clause for the attach error, from the sweep result and the page probe
 *  (`blockers`: tabs that did not answer DevTools). Never advises a stop: a
 *  restart restores the same tabs. */
export function describe(sweep, blockers = []) {
  const r = sweep || { state: 'failed', detail: 'no sweep' };
  // at most three names, so the remedy survives the error's clip
  const named = titles(blockers.slice(0, 3).map((b) => b.title ? `${b.title} — ${b.host}` : b.host)) + (blockers.length > 3 ? ` and ${blockers.length - 3} more` : '');
  if (r.left_count > 0) return `${r.left_count} restored tab(s) could not be loaded (${titles(r.left)}); ask the user to click them once in the herdr+ Browser window, then retry`;
  // a tab mid-load does not answer the probe either: with tabs still loading, no blame on a dialog alone
  if (r.loading > 0 && blockers.length) return `these tabs are not responding (${named}) and ${r.loading} tab(s) are still loading; answer a dialog if one is open in the herdr+ Browser window, else retry in a few seconds`;
  if (r.loading > 0) return `${r.loading} tab(s) are still loading (${titles(r.loading_titles || [])}); retry in a few seconds`;
  if (blockers.length) return `these tabs are not responding (a dialog may be open in one): ${named}; answer or close it in the herdr+ Browser window, then retry`;
  if (r.state !== 'ready') return `the companion extension is not reachable (${r.state}${r.detail ? ': ' + r.detail : ''}), so restored tabs could not be loaded; ask the user to click the restored tabs once in the herdr+ Browser window, then retry`;
  return `${r.reloaded || 0} restored tab(s) were loaded, so a page that is not answering is blocking — a dialog (alert, 'Leave site?') or a frozen page; ask the user to look at the herdr+ Browser window, then retry`;
}

/** Which page targets answer DevTools (raw CDP, one `Runtime.evaluate` per
 *  page, `capMs` each, all in parallel): the ones that do not are what blocks
 *  a connect — a dialog, a frozen renderer. Read-only. `list` is
 *  `/json/list` (fetched when not given); `open` makes a WebSocket (for
 *  tests). */
export async function probePages(port, capMs = 1000, list = null, open = (url) => new WebSocket(url), deadlineAt = Infinity) {
  // everything inside what is left of the attach deadline: the list fetch, then the probes
  const remaining = () => Math.max(0, deadlineAt - Date.now());
  if (remaining() === 0) return { pages: 0, blocked: [], skipped: 'no time left' };
  const targets = list || await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(Math.max(1, Math.min(1000, remaining()))) }).then((r) => r.json()).catch(() => []);
  const pages = targets.filter((t) => t.type === 'page' && t.webSocketDebuggerUrl);
  const host = (url) => { try { return new URL(url).host || url.slice(0, 40); } catch { return String(url).slice(0, 40); } };
  const probeMs = Math.min(capMs, remaining());
  if (probeMs === 0) return { pages: pages.length, blocked: [], skipped: 'no time left' };
  const probe = (t) => new Promise((resolve) => {
    let ws; let timer = null; let settled = false;
    // once: the first verdict counts (a close caused by our own close() is not a second one)
    const done = (ok) => { if (settled) return; settled = true; clearTimeout(timer); try { if (ws) { ws.onclose = null; ws.close(); } } catch {} resolve(ok); };
    timer = setTimeout(() => done(false), probeMs);
    try { ws = open(t.webSocketDebuggerUrl); } catch { return done(false); }
    ws.onopen = () => ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression: '1', returnByValue: true } }));
    ws.onmessage = (m) => { let msg; try { msg = JSON.parse(m.data); } catch { return; } if (msg.id === 1) done(true); };
    ws.onerror = () => done(false);
    ws.onclose = () => done(false);
  });
  const answers = await Promise.all(pages.map(probe));
  const blocked = pages.filter((_, i) => !answers[i]).map((t) => ({ title: String(t.title || '').slice(0, 60), host: host(t.url || '') }));
  return { pages: pages.length, blocked };
}

/** Wake an idled companion worker with a tab event before anything is
 *  attached: raw CDP on the browser endpoint (`/json/version`), a blank
 *  target created in the background and closed again. Bounded by `capMs`
 *  in total; never raises the window (measured). `open` makes the
 *  WebSocket (for tests). */
export async function rawNudge(port, capMs = 1500, open = (url) => new WebSocket(url)) {
  const t0 = Date.now();
  const version = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(Math.max(1, capMs)) }).then((r) => r.json());
  const ws = open(version.webSocketDebuggerUrl);
  const pending = new Map();
  let id = 0;
  const send = (method, params, ms = Math.max(1, capMs - (Date.now() - t0))) => new Promise((resolve, reject) => {
    const n = ++id;
    const timer = setTimeout(() => { pending.delete(n); reject(new Error(`${method} took too long`)); }, ms);
    pending.set(n, (msg) => { clearTimeout(timer); msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result || {}); });
    ws.send(JSON.stringify({ id: n, method, params }));
  });
  try {
    await withTimeout(new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('browser endpoint websocket failed')); }), Math.max(1, capMs - (Date.now() - t0)), 'nudge connect');
    ws.onmessage = (m) => { let msg; try { msg = JSON.parse(m.data); } catch { return; } const waiter = pending.get(msg.id); if (waiter) { pending.delete(msg.id); waiter(msg); } };
    const { targetId } = await send('Target.createTarget', { url: NUDGE_URL, background: true });
    await sleep(50);
    // its own second, outside the cap: the tab is closed even when the cap is spent (the next sweep removes what is left)
    await send('Target.closeTarget', { targetId }, 1000);
  } finally {
    try { ws.close(); } catch {}
  }
}

/** The companion extension of one profile: the worker connection, target → tab ids, the pane groups. */
export class Companion {
  constructor(profile) {
    this.profile = profile;
    this.ws = null;
    this.pending = new Map();
    this.id = 0;
    this.state = 'unknown';
    this.detail = '';
    this.tabIds = new Map(); // targetId -> extension tab id
    this.groups = new Map(); // pane key -> { tabs: Set<tabId>, timer } (the group id lives in the worker's session storage)
    this.userUngrouped = new Set();
    this.chain = Promise.resolve();
    this.connecting = null;
    this.log = () => {};
    this.stale = false; // the running worker is older than this herdr's files
    this.scope = null; // chrome-extension://<id>/
    this.gen = 0; // bumped by close(): a worker wait from an earlier browser gives up
  }
  info() { return { state: this.state, detail: this.detail }; }
  /** A DevTools session on a page of the profile over a connection of its
   *  own: the `ServiceWorker` domain is only served on page targets (not on
   *  the browser target), and an unregister sent through the sidecar's
   *  long-lived connection never took while the same calls over a fresh
   *  `connectOverCDP` did (live, repeatedly). Answers { session, close }. */
  async pageSession() {
    // (loaded here, not at the top: scripts/test_browser_companion.mjs imports this file from the source tree)
    const { chromium } = await import('playwright-core');
    const browser = await chromium.connectOverCDP(`http://127.0.0.1:${this.profile.port}`, { timeout: COMPANION_CALL_MS });
    const close = () => browser.close().catch(() => {});
    const ctx = browser.contexts()[0];
    const pages = ctx ? ctx.pages() : [];
    // a page outside the extension's own scope first (what worked live)
    const page = pages.find((p) => !this.scope || !p.url().startsWith(this.scope)) || pages[0];
    if (!page) { await close(); return null; }
    const session = await ctx.newCDPSession(page);
    return { session, close };
  }
  /** Chrome keeps an unpacked command-line extension's worker script for good:
   *  a manifest version bump, a browser restart, `chrome.runtime.reload()`
   *  and `importScripts` of a new URL do not refresh it, and a worker that is
   *  unregistered at runtime does not come back until the next browser start.
   *  So a stale worker keeps serving this session (reported as pending), and
   *  right before herdr closes the browser its registration is removed and
   *  the worker stopped — together, in one DevTools session: an unregister
   *  alone never completed while the worker ran, and a worker that was still
   *  running at shutdown came back with the old script. After that the next
   *  start registers the files on disk afresh (verified live). The
   *  `ServiceWorker` domain is only served on page targets, hence the page
   *  session; nothing on disk is touched. */
  async stopStaleWorker() {
    if (!this.stale || !this.scope) return false;
    const link = await this.pageSession();
    if (!link) { this.log('companion: no page for a DevTools session; the stale worker stays'); return false; }
    const { session } = link;
    const versions = new Map(); // versionId -> runningStatus
    let deleted = false;
    const onVersions = ({ versions: list }) => { for (const v of list) if (v.scriptURL.startsWith(this.scope)) versions.set(v.versionId, v.runningStatus); };
    const onRegistrations = ({ registrations }) => { for (const r of registrations) if (r.scopeURL === this.scope && r.isDeleted) deleted = true; };
    const live = () => [...versions].filter(([, status]) => status === 'running' || status === 'starting').map(([id]) => id);
    session.on('ServiceWorker.workerVersionUpdated', onVersions);
    session.on('ServiceWorker.workerRegistrationUpdated', onRegistrations);
    try {
      await withTimeout(session.send('ServiceWorker.enable'), COMPANION_CALL_MS, 'ServiceWorker.enable');
      await sleep(700); // the domain's initial dump settles (an unregister sent right after enable never took)
      const running = live();
      // (the held worker connection stays open until the worker is gone: a
      // registration whose worker lost its DevTools session first was never
      // marked deleted)
      await withTimeout(session.send('ServiceWorker.unregister', { scopeURL: this.scope }), COMPANION_CALL_MS, 'ServiceWorker.unregister');
      for (const versionId of running) await session.send('ServiceWorker.stopWorker', { versionId }).catch(() => {});
      for (let i = 0; i < 20 && !(deleted && running.every((id) => versions.get(id) === 'stopped')); i++) await sleep(100);
      this.close();
      this.log(`companion: stale worker retired (registration ${deleted ? 'deleted' : 'still listed'}, ${running.length} stopped); the next browser start loads the new files`);
      return deleted;
    } finally {
      session.off('ServiceWorker.workerVersionUpdated', onVersions);
      session.off('ServiceWorker.workerRegistrationUpdated', onRegistrations);
      await session.detach().catch(() => {});
      await link.close();
    }
  }
  async targets(capMs = 2000) {
    const res = await fetch(`http://127.0.0.1:${this.profile.port}/json/list`, { signal: AbortSignal.timeout(Math.max(1, Math.min(2000, capMs))) });
    return res.json();
  }
  findIn(list) { return list.find((t) => t.type === 'service_worker' && /\/sw\.js$/.test(t.url)) || null; }
  /** Before a connect: reload every tab Chrome restored but never loaded
   *  (they never answer DevTools and hang connectOverCDP), then wait for the
   *  reloaded ones to settle (tabs mid-load stall the connect briefly). Waits
   *  up to SWEEP_WORKER_WAIT_MS for the worker right after a launch. Never
   *  throws; the result goes into the attach's log and error text. */
  async sweep(budgetMs = 8000) {
    const t0 = Date.now();
    const left = () => budgetMs - (Date.now() - t0);
    if (typeof WebSocket !== 'function') return { state: 'unsupported', detail: 'node has no WebSocket' };
    const listed = async () => this.findIn(await this.targets(left()).catch(() => []));
    let sw = null;
    while (!(sw = await listed()) && Date.now() - t0 < Math.min(SWEEP_WORKER_WAIT_MS, budgetMs)) await sleep(200);
    if (!sw && left() > 500) {
      // Chrome idled the worker out: a tab event wakes it (a blank target
      // opened in the background and closed again, raw CDP on the browser
      // endpoint — nothing Playwright, nothing that raises the window)
      await rawNudge(this.profile.port, Math.min(1500, left())).catch((err) => this.log(`companion nudge: ${err.message}`));
      const until = Date.now() + Math.min(1500, Math.max(0, left()));
      while (!(sw = await listed()) && Date.now() < until) await sleep(200);
    }
    if (!sw) return { state: 'missing', detail: 'no companion service worker on the DevTools port', ms: Date.now() - t0 };
    let result;
    try {
      result = Object.assign({ state: 'ready' }, await withTimeout(this.evaluate(SWEEP_EXPR, 'sweep'), Math.max(500, left()), 'companion sweep'));
    } catch (err) {
      return { state: 'failed', detail: String(err.message).slice(0, 120), ms: Date.now() - t0 };
    }
    // the worker is ours from here: what probe() establishes (state, scope, the version)
    await this.adopt(sw, Math.max(300, Math.min(1000, left()))).catch(() => {});
    // settle on what is loading (restored tabs mid-load stall the connect):
    // one poll always, more while something loads and the budget lasts; a
    // failed poll keeps the last known count
    const settleUntil = Date.now() + Math.min(SWEEP_SETTLE_MS, Math.max(0, left()));
    let loading = { loading: 0, loading_titles: [] };
    for (;;) {
      loading = await withTimeout(this.evaluate(LOADING_EXPR, 'sweep settle'), Math.max(300, left()), 'companion settle').catch(() => loading);
      if (!(loading.loading > 0) || Date.now() + 250 >= settleUntil) break;
      await sleep(250);
    }
    return Object.assign(result, loading, { ms: Date.now() - t0 });
  }
  /** The listed worker is ours: state ready, its scope, and whether it runs
   *  this herdr's code (an older worker is reported as pending; see
   *  `stopStaleWorker`). Shared by `probe` and `sweep`. */
  async adopt(sw, pingMs = COMPANION_CALL_MS) {
    this.state = 'ready';
    this.detail = '';
    this.stale = false;
    // (not URL.origin: Node answers "null" for chrome-extension: URLs)
    this.scope = sw.url.slice(0, sw.url.lastIndexOf('/') + 1);
    // stale only when the worker answered: another version string, or an
    // evaluation error (a worker without herdrPing); a timed-out ping leaves
    // the version unverified and the worker current
    let ping;
    try {
      ping = String(await withTimeout(this.call('herdrPing'), pingMs, 'companion ping'));
    } catch (err) {
      if (/took more than/.test(String(err && err.message))) return this.info();
      ping = '';
    }
    const version = Number((ping.split('/')[1] || '0'));
    if (version !== COMPANION_VERSION) {
      this.stale = true; // retired by stopStaleWorker when herdr closes the browser
      this.detail = `worker v${version || '?'}, this herdr expects v${COMPANION_VERSION} — extension update pending: \`herdr browser stop\` and open again`;
    }
    return this.info();
  }
  /** Is the extension loaded, and current? (at attach) A worker still running
   *  older code (the files were refreshed under it) keeps serving this
   *  session and is reported as pending; see `stopStaleWorker`. */
  async probe() {
    this.stale = false;
    this.scope = null;
    if (typeof WebSocket !== 'function') { this.state = 'unsupported'; this.detail = 'node has no WebSocket'; return this.info(); }
    try {
      const sw = await this.worker(true);
      if (!sw) { this.state = 'missing'; this.detail = 'no companion service worker on the DevTools port'; return this.info(); }
      await this.adopt(sw);
    } catch (err) {
      this.state = 'missing';
      this.detail = String(err.message).slice(0, 120);
    }
    return this.info();
  }
  /** The worker target, woken by a tab event when Chrome idled it out. */
  async worker(wake) {
    const gen = this.gen;
    let sw = this.findIn(await this.targets());
    if (!sw && wake) {
      await this.nudge();
      for (let i = 0; i < 15 && !sw && gen === this.gen && this.profile.browser; i++) { await sleep(100); sw = this.findIn(await this.targets()); }
    }
    return sw;
  }
  async nudge() {
    const browser = this.profile.browser;
    if (!browser) return;
    let session = null;
    try {
      session = await browser.newBrowserCDPSession();
      const { targetId } = await session.send('Target.createTarget', { url: NUDGE_URL, background: true });
      await sleep(50);
      await session.send('Target.closeTarget', { targetId });
    } catch (err) {
      this.log(`companion nudge: ${err.message}`);
    } finally {
      if (session) await session.detach().catch(() => {});
    }
  }
  /** One connection at a time: concurrent callers share the in-flight attempt. */
  connect() {
    if (this.ws && this.ws.readyState === 1) return Promise.resolve(this.ws);
    if (this.connecting) return this.connecting;
    this.connecting = this._connect().finally(() => { this.connecting = null; });
    return this.connecting;
  }
  async _connect() {
    // A worker Chrome idled out between the keep-alive alarms is a transient
    // miss: the call fails, `state` stays what `probe` found, the next call
    // retries (a latched 'missing' here once turned every group and page
    // push off until the next attach).
    const sw = await this.worker(true);
    if (!sw) throw new Error('companion: no service worker right now (idle?); retrying on the next call');
    const ws = new WebSocket(sw.webSocketDebuggerUrl);
    try {
      await withTimeout(new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('companion: websocket failed')); }), COMPANION_CALL_MS, 'companion connect');
    } catch (err) {
      try { ws.close(); } catch {}
      throw err;
    }
    ws.onmessage = (m) => {
      let msg; try { msg = JSON.parse(m.data); } catch { return; }
      const waiter = this.pending.get(msg.id);
      if (waiter) { this.pending.delete(msg.id); waiter(msg); }
    };
    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      for (const waiter of this.pending.values()) waiter({ error: { message: 'companion connection closed' } });
      this.pending.clear();
    };
    ws.onerror = () => {};
    this.ws = ws;
    this.state = 'ready';
    return ws;
  }
  async call(fn, arg) {
    return this.evaluate(`${fn}(${JSON.stringify(arg === undefined ? null : arg)})`, fn);
  }
  /** One expression in the worker (`chrome.*` is at hand there); `what` names it in errors. */
  async evaluate(expression, what) {
    const ws = await this.connect();
    const id = ++this.id;
    const reply = await withTimeout(new Promise((resolve) => {
      this.pending.set(id, resolve);
      ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
    }), COMPANION_CALL_MS, `companion ${what}`).finally(() => this.pending.delete(id));
    if (reply.error) throw new Error(reply.error.message || 'companion call failed');
    const details = reply.result && reply.result.exceptionDetails;
    if (details) throw new Error(details.exception && details.exception.description || details.text || 'companion call failed');
    return reply.result && reply.result.result ? reply.result.result.value : undefined;
  }
  /** Right after `Target.createTarget('about:blank')`: the newest blank tab not yet known is this target's. */
  async adoptNew(targetId) {
    const tabs = await this.call('herdrTabs');
    const known = new Set(this.tabIds.values());
    const candidates = tabs.filter((t) => t.url === 'about:blank' && !known.has(t.id)).sort((a, b) => b.id - a.id);
    if (candidates.length) this.tabIds.set(targetId, candidates[0].id);
    else this.log(`companion: no blank tab to adopt for ${targetId}`);
  }
  /** The extension tab id of a tracked page: cached, else the one tab with its URL (and title). */
  async tabIdFor(state, quiet = false) {
    const cached = this.tabIds.get(state.target);
    if (cached != null) return cached;
    const url = state.page.url();
    const tabs = await this.call('herdrTabs');
    const known = new Set(this.tabIds.values());
    let candidates = tabs.filter((t) => t.url === url && !known.has(t.id));
    if (candidates.length > 1) {
      const title = await state.page.title().catch(() => '');
      candidates = candidates.filter((t) => t.title === title);
    }
    if (candidates.length !== 1) { if (!quiet) this.log(`companion: ${candidates.length} tabs match ${url}; not grouped`); return null; }
    this.tabIds.set(state.target, candidates[0].id);
    return candidates[0].id;
  }
  /** Make the page's tab the active tab of its window, quietly: the worker's
   *  `chrome.tabs.update(id, { active: true })` selects the tab without
   *  raising the app (no `windows.update({ focused })`, no bringToFront —
   *  measured: no frontmost-app change, and a stalled background tab paints
   *  again). False when the companion or the tab id is not at hand; the
   *  window is never raised from here. */
  async select(state) {
    if (this.state !== 'ready') return false;
    const tabId = await this.tabIdFor(state, true);
    if (tabId == null) return false;
    await this.evaluate(`chrome.tabs.update(${Number(tabId)}, { active: true })`, 'select');
    return true;
  }
  forget(targetId) {
    const tabId = this.tabIds.get(targetId);
    this.tabIds.delete(targetId);
    if (tabId != null) for (const g of this.groups.values()) g.tabs.delete(tabId);
  }
  /** An agent pane touched a tab: it sits in the pane's group, expanded and
   *  marked active in its title (`● <title>`, by the worker); after
   *  `collapse_ms` of quiet the group collapses and the mark goes. */
  touch(state, group) {
    const run = this.chain.then(() => this._touch(state, group));
    this.chain = run.catch((err) => this.log(`companion: ${err.message}`));
    return this.chain;
  }
  async _touch(state, group) {
    if (this.state === 'unsupported' || !group || !group.key) return;
    // The directive's window as it is; missing or not a number: the default.
    // 0 is off: no group expanded or marked for the pane, no collapse later
    // (and, without a companion flag for "join quietly", no grouping either).
    const ms = Overlay.window(group.collapse_ms);
    if (ms === 0) return;
    const tabId = await this.tabIdFor(state);
    if (tabId == null || this.userUngrouped.has(tabId)) return;
    let g = this.groups.get(group.key);
    if (!g) { g = { tabs: new Set(), timer: null }; this.groups.set(group.key, g); }
    const wasGrouped = g.tabs.has(tabId);
    let reply;
    try {
      // The group is the pane's (keyed by its id in the worker's session storage), never one found by title.
      reply = await this.call('herdrGroup', { key: String(group.key), tabId, title: String(group.title || group.key), color: String(group.color || 'purple'), wasGrouped, active: true });
    } catch (err) {
      if (/No tab with id/i.test(err.message)) { this.tabIds.delete(state.target); g.tabs.delete(tabId); }
      throw err;
    }
    if (reply && reply.skipped === 'user_ungrouped') { this.userUngrouped.add(tabId); g.tabs.delete(tabId); return; }
    if (!reply || reply.skipped) return;
    g.tabs.add(tabId);
    if (g.timer) clearTimeout(g.timer);
    g.timer = setTimeout(() => {
      g.timer = null;
      // (a stale v10 worker reads a bare key only; it keeps the plain title anyway)
      const arg = this.stale ? String(group.key) : { key: String(group.key), title: String(group.title || group.key) };
      this.call('herdrCollapse', arg).catch((err) => this.log(`companion collapse: ${err.message}`));
    }, ms);
    if (g.timer.unref) g.timer.unref();
  }
  /** The pinned dashboard: on in every normal window, or removed. */
  dashboard(pin) {
    return this.call('herdrDashboard', { pin: Boolean(pin) });
  }
  /** Panes that are gone: the overlay on each of the pane's tabs goes now and
   *  the group recorded for each key dissolves (only that group; a same-named
   *  group of the user's or another pane's is never touched). Throws when the
   *  worker could not do it. */
  async release(keys) {
    for (const key of keys) {
      const g = this.groups.get(String(key));
      if (g) {
        if (g.timer) clearTimeout(g.timer);
        const pages = this.profile.pages;
        if (pages) {
          for (const [target, tabId] of this.tabIds) {
            const state = g.tabs.has(tabId) ? pages.get(target) : null;
            if (state && state.overlay) state.overlay.dismiss();
          }
        }
        this.groups.delete(String(key));
      }
      await this.call('herdrRelease', String(key));
    }
  }
  /** The browser went away (or is being closed): drop everything that
   *  belonged to it. An in-flight connect keeps failing on its own; the next
   *  attach's calls must not wait on it (one probe timed out that way). */
  close() {
    for (const g of this.groups.values()) if (g.timer) clearTimeout(g.timer);
    this.groups.clear();
    this.tabIds.clear();
    if (this.ws) { try { this.ws.close(); } catch {} this.ws = null; }
    this.connecting = null;
    this.gen++;
    this.stale = false;
    this.scope = null;
  }
}

// ---------------------------------------------------------------------------
// Fork (companion browser): the companion's control page. An Electron app
// registered as the browser host (`attach` with kind "companion") is driven
// through `window.__herdrCompanion` on its always-hidden control page
// (`herdr-companion://control/`): open/select/focus/close/show views. Its
// own pages (`herdr-companion:` scheme) are furniture, never tabs.

export const CONTROL_URL_PREFIX = 'herdr-companion://control/';
export const CONTROL_CALL_MS = 5000;

/** The companion's own pages (control page, strip): never adopted, tracked or counted. */
export function isFurnitureUrl(url) {
  return typeof url === 'string' && url.startsWith('herdr-companion:');
}

/** The control page driver. `pages()` lists the candidate pages (every
 *  context's); `evaluate(page, fn, arg)` runs in the page. Errors the page
 *  answers (`{error:{code,message}}`) throw with that code. */
export class ControlPage {
  constructor({ pages, evaluate, callMs = CONTROL_CALL_MS }) {
    this.pages = pages;
    this.evaluate = evaluate;
    this.callMs = callMs;
    this.page = null;
    this.views = new Map(); // target id -> view id
  }
  find() {
    if (this.page && !(this.page.isClosed && this.page.isClosed())) return this.page;
    this.page = this.pages().find((page) => String(page.url()).startsWith(CONTROL_URL_PREFIX)) || null;
    return this.page;
  }
  async call(fn, arg) {
    const page = this.find();
    if (!page) throw Object.assign(new Error('the companion control page is gone'), { herdr: true, code: 'host_verify_failed' });
    let timer;
    const timeout = new Promise((_, reject) => { timer = setTimeout(() => reject(Object.assign(new Error(`companion ${fn} did not answer in ${this.callMs} ms`), { herdr: true, code: 'browser_timeout' })), this.callMs); });
    try {
      const out = await Promise.race([
        this.evaluate(page, ([name, value]) => {
          const api = globalThis.__herdrCompanion;
          // `missing`: a companion older than the call (its other calls work)
          if (!api || typeof api[name] !== 'function') return { error: { herdr: true, code: 'host_verify_failed', message: `the control page has no ${name}()`, missing: Boolean(api) } };
          return api[name](value);
        }, [fn, arg === undefined ? null : arg]),
        timeout,
      ]);
      if (out && typeof out === 'object' && out.error) {
        throw Object.assign(new Error(String(out.error.message || out.error.code)), { herdr: true, code: String(out.error.code || 'browser_error'), missing: Boolean(out.error.missing) });
      }
      return out;
    } finally { clearTimeout(timer); }
  }
  /** Focus a view (`browser focus`, the login handoff). An agent's focus goes as the bare
   *  view id, which every companion reads as a focus (one from before the user's own kind
   *  activates its window, as it always did); the user's own as `{view_id, user: true}`,
   *  sent bare again to a companion that takes the object for an unknown view. */
  async focusView(view, user) {
    if (!user) return this.call('focus', view);
    try {
      return await this.call('focus', { view_id: view, user: true });
    } catch (err) {
      if (err.code !== 'view_not_found') throw err;
      return this.call('focus', view);
    }
  }
  /** Raise the window (herdr's Focus companion; `user`: with the keyboard). A companion
   *  from before `raise()`: the user's raise focuses its shown page, which activates it; an
   *  agent's cannot happen there without taking the keyboard. */
  async raise(user) {
    try {
      return await this.call('raise', { user: Boolean(user) });
    } catch (err) {
      if (!err.missing) throw err;
      if (user) {
        const views = await this.call('views').catch(() => []);
        const list = Array.isArray(views) ? views : [];
        const shown = list.find((view) => view && view.shown) || list.find((view) => view && view.active);
        if (shown && shown.view_id) return this.call('focus', shown.view_id);
      }
      throw Object.assign(new Error('this companion browser predates herdr\'s companion controls; quit it and let herdr start it'), { herdr: true, code: 'companion_outdated' });
    }
  }
  /** The control page answers the registration's nonce. */
  async verify(nonce) {
    let got = null;
    try { got = await this.call('nonce'); } catch (err) { throw Object.assign(new Error(`the companion did not answer its nonce: ${err.message}`), { herdr: true, code: 'host_verify_failed' }); }
    if (got !== nonce) throw Object.assign(new Error('the companion on that port is not the one that registered (nonce mismatch)'), { herdr: true, code: 'host_verify_failed' });
    return true;
  }
  remember(target, view) { if (target && view) this.views.set(String(target), String(view)); }
  forget(target) { this.views.delete(String(target)); }
  /** A target's view id (asks the companion once when unknown). */
  async viewOf(target) {
    if (this.views.has(target)) return this.views.get(target);
    const views = await this.call('views').catch(() => []);
    for (const view of Array.isArray(views) ? views : []) {
      if (view && view.target_id && view.view_id) this.remember(view.target_id, view.view_id);
    }
    if (!this.views.has(target)) throw Object.assign(new Error('the companion does not know this tab'), { herdr: true, code: 'tab_closed' });
    return this.views.get(target);
  }
}

/** Fork (companion dialogs): a page's alert / confirm / prompt in the
 *  companion. Electron has no prompt() and the companion shows all three in
 *  its own card, so no CDP Page.javascriptDialogOpening comes; the companion
 *  pushes its open dialogs (`{seq, dialogs: [{id, view_id, target_id, type,
 *  message, default_value}]}`) to a binding on its control page and answers
 *  through `dialog({view_id, id, accept, text})`. This stands in for
 *  Playwright's Dialog, so the sidecar's dialog paths (events, guard,
 *  `browser dialog accept|dismiss [text]`) stay as they are. An accept
 *  without text keeps what the field holds (the default unless the user
 *  typed). */
export const DIALOG_BINDING = '__herdrDialogs';
export class CompanionDialog {
  constructor(control, d, onAnswer = () => {}, onUnanswered = () => {}) {
    this.control = control;
    this.companion = true;
    this.id = String(d.id);
    this.viewId = String(d.view_id);
    this._type = ['alert', 'confirm', 'prompt'].includes(d.type) ? d.type : 'alert';
    this._message = String(d.message == null ? '' : d.message);
    this._default = String(d.default_value == null ? '' : d.default_value);
    this.onAnswer = onAnswer;
    this.onUnanswered = onUnanswered;
  }
  type() { return this._type; }
  message() { return this._message; }
  defaultValue() { return this._default; }
  accept(text) { return this._answer(true, text); }
  dismiss() { return this._answer(false); }
  // Remembered as answered while the call goes (a push from before it must not bring the
  // dialog back); forgotten again when the companion did not take it (a timeout, the
  // control page gone): only no_dialog means it is closed.
  async _answer(accept, text) {
    this.onAnswer(this.id);
    const arg = { view_id: this.viewId, id: this.id, accept };
    if (typeof text === 'string') arg.text = text;
    try {
      await this.control.call('dialog', arg);
    } catch (err) {
      if (!err || err.code !== 'no_dialog') this.onUnanswered(this.id);
      throw err;
    }
  }
}

/** The answered ids a push still needs: those it lists (an answer in flight).
 *  Ids never repeat, so one the companion no longer lists is gone for good. */
export function pruneAnswered(answered, list) {
  const listed = new Set((Array.isArray(list) ? list : []).filter((d) => d && d.id).map((d) => String(d.id)));
  for (const id of [...answered]) if (!listed.has(id)) answered.delete(id);
  return answered;
}

/** What a companion dialog push changes for the tracked pages: `current`
 *  is target -> the page's CompanionDialog (or null), `list` the pushed
 *  dialogs; `answered` the ids already answered here (a push from before the
 *  answer must not bring one back). Answers `{ open: [[target, d]], close:
 *  [target] }`. */
export function reconcileDialogs(current, list, answered = new Set()) {
  const pushed = new Map();
  for (const d of Array.isArray(list) ? list : []) {
    if (d && d.target_id && d.id && !answered.has(String(d.id))) pushed.set(String(d.target_id), d);
  }
  const open = [];
  const close = [];
  for (const [target, have] of current) {
    const d = pushed.get(target);
    if (d && (!have || have.id !== String(d.id))) open.push([target, d]);
    else if (!d && have) close.push(target);
  }
  return { open, close };
}

/** File watches for HTML views: the file and its directory (not
 *  recursive), debounced, then `reload()`. At most `max` views. `watchFn`
 *  is `fs.watch`. */
export class ViewWatches {
  constructor({ watchFn, debounceMs = 200, max = 32, log = () => {} }) {
    this.watchFn = watchFn;
    this.debounceMs = debounceMs;
    this.max = max;
    this.log = log;
    this.watches = new Map(); // target -> { watchers, timer }
  }
  arm(target, file, reload) {
    if (!this.watches.has(target) && this.watches.size >= this.max) {
      throw Object.assign(new Error(`at most ${this.max} views are watched; close some`), { herdr: true, code: 'too_many_watches' });
    }
    this.disarm(target);
    const entry = { watchers: [], timer: null };
    const fire = () => {
      clearTimeout(entry.timer);
      entry.timer = setTimeout(() => { Promise.resolve().then(reload).catch((err) => this.log(`reload: ${err && err.message}`)); }, this.debounceMs);
    };
    const slash = Math.max(file.lastIndexOf('/'), file.lastIndexOf('\\'));
    const dir = slash > 0 ? file.slice(0, slash) : '.';
    for (const path of [file, dir]) {
      try { entry.watchers.push(this.watchFn(path, fire)); } catch (err) { this.log(`watch ${path}: ${err && err.message}`); }
    }
    this.watches.set(target, entry);
    return entry.watchers.length > 0;
  }
  disarm(target) {
    const entry = this.watches.get(target);
    if (!entry) return false;
    clearTimeout(entry.timer);
    for (const watcher of entry.watchers) { try { watcher.close(); } catch {} }
    this.watches.delete(target);
    return true;
  }
  has(target) { return this.watches.has(target); }
}
