// herdr browser: main-content → markdown, run inside the page with page.evaluate.
// No dependencies (no Readability, no Turndown): headings, paragraphs, lists,
// pipe tables, links as [text](href), images as ![alt], code, quotes; drops
// nav/footer/aside/script/style/template/noscript, hidden and aria-hidden nodes.
// An open dialog (<dialog open>, a visible role=dialog / alertdialog /
// aria-modal element: a Vue or React modal) comes first as a `## Dialog`
// block, even when the rest is limited to <main>.

// Visible dialogs of the page, outermost only (in-page; shared by the
// extraction and the act probe). A role=dialog without aria-modal counts too
// for the extraction; the act probe asks for modal ones (`modalOnly`).
const DIALOG_FINDER = String.raw`function findDialogs(modalOnly) {
    const SEL = modalOnly
      ? 'dialog[open], [role="dialog"][aria-modal="true"], [role="alertdialog"], [aria-modal="true"]'
      : 'dialog[open], [role="dialog"], [role="alertdialog"], [aria-modal="true"]';
    const shown = (el) => {
      if (el.getAttribute('aria-hidden') === 'true' || el.closest('[aria-hidden="true"]')) return false;
      if (typeof el.checkVisibility === 'function') { if (!el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) return false; }
      else { const st = getComputedStyle(el); if (st.display === 'none' || st.visibility === 'hidden') return false; }
      const r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    };
    const all = Array.from(document.querySelectorAll(SEL)).filter(shown);
    return all.filter((el) => !all.some((other) => other !== el && other.contains(el)));
  }
  function dialogTitle(el) {
    const ids = (el.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
    const labelled = ids.map((id) => { const n = document.getElementById(id); return n ? n.textContent : ''; }).join(' ');
    const heading = el.querySelector('h1, h2, h3, h4, h5, h6, [role="heading"]');
    const t = labelled.trim() || el.getAttribute('aria-label') || (heading && heading.textContent) || '';
    return t.replace(/\s+/g, ' ').trim().slice(0, 120);
  }
  function dialogButtons(el) {
    const out = [];
    for (const b of el.querySelectorAll('button, [role="button"], input[type="button"], input[type="submit"], a[role="button"]')) {
      const r = b.getBoundingClientRect();
      if (!(r.width > 0 && r.height > 0)) continue;
      const label = (b.getAttribute('aria-label') || b.innerText || b.value || b.getAttribute('title') || '').replace(/\s+/g, ' ').trim().slice(0, 40);
      if (label && !out.includes(label)) out.push(label);
      if (out.length >= 8) break;
    }
    return out;
  }`;

// The open modal dialogs and the page's title and focused element, for the
// act's "what changed" (before and after a click). `acted` (optional): the
// element the act clicked or pressed; `focused.acted` says the focus is on it
// (or on the control a clicked label targets).
export const MODALS_SOURCE = String.raw`(function (acted) {
  ` + DIALOG_FINDER + String.raw`
  const modals = findDialogs(true).map((el) => ({ title: dialogTitle(el), buttons: dialogButtons(el) }));
  let el = document.activeElement;
  while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
  let focused = null;
  if (el && el !== document.body && el !== document.documentElement) {
    const tag = el.tagName.toLowerCase();
    const editable = /^(input|textarea|select)$/.test(tag) || el.isContentEditable === true;
    const role = el.getAttribute('role') || (tag === 'a' ? 'link' : tag === 'button' ? 'button' : editable ? 'textbox' : tag);
    // never a field's contents
    const name = (el.getAttribute('aria-label') || (el.labels && el.labels[0] && el.labels[0].innerText) || el.placeholder || (editable ? '' : el.innerText) || el.getAttribute('title') || '').replace(/\s+/g, ' ').trim().slice(0, 60);
    focused = { role, name };
    if (acted && (el === acted || acted.control === el)) focused.acted = true;
  }
  return { modals, title: document.title, focused };
})`;

export const EXTRACT_SOURCE = String.raw`(function (args) {
  const opts = args || {};
  ` + DIALOG_FINDER + String.raw`
  // A closed <dialog> is display:none (hidden() drops it); an open one is content.
  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'CANVAS', 'IFRAME', 'OBJECT', 'EMBED', 'VIDEO', 'AUDIO', 'MAP']);
  const CHROME = new Set(['NAV', 'FOOTER', 'ASIDE']);
  const BLOCK = new Set(['P', 'DIV', 'SECTION', 'ARTICLE', 'MAIN', 'HEADER', 'ASIDE', 'NAV', 'FOOTER', 'UL', 'OL', 'LI', 'TABLE', 'THEAD', 'TBODY', 'TR', 'BLOCKQUOTE', 'PRE', 'HR', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'DL', 'DT', 'DD', 'FORM', 'FIELDSET', 'FIGURE', 'FIGCAPTION', 'DETAILS', 'SUMMARY', 'ADDRESS']);
  function hidden(el) {
    if (!(el instanceof Element)) return false;
    if (el.getAttribute('aria-hidden') === 'true' || el.hidden) return true;
    const style = el.ownerDocument.defaultView.getComputedStyle(el);
    return style.display === 'none' || style.visibility === 'hidden';
  }
  function text(node) {
    return (node.textContent || '').replace(/\s+/g, ' ');
  }
  function escapeCell(s) { return s.replace(/\|/g, '\\|').trim(); }
  function abs(href) { try { return new URL(href, document.baseURI).href; } catch { return href; } }
  function render(node, ctx) {
    if (node.nodeType === Node.TEXT_NODE) {
      const raw = node.nodeValue || '';
      return ctx.pre ? raw : raw.replace(/\s+/g, ' ');
    }
    if (node.nodeType !== Node.ELEMENT_NODE) return '';
    const el = node;
    const tag = el.tagName;
    if (SKIP.has(tag) || hidden(el)) return '';
    if (tag === 'DIALOG' && !el.open) return '';
    // a dialog already rendered at the top is not repeated in the body
    if (!ctx.dialog && dialogSet.has(el)) return '';
    if (!ctx.inMain && CHROME.has(tag)) return '';
    if (tag === 'BR') return ctx.pre ? '\n' : '  \n';
    if (tag === 'HR') return '\n\n---\n\n';
    if (tag === 'IMG') {
      const alt = (el.getAttribute('alt') || '').trim();
      return alt ? '![' + alt + ']' : '';
    }
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
      const label = el.getAttribute('aria-label') || el.getAttribute('placeholder') || el.getAttribute('name') || '';
      const type = el.getAttribute('type') || tag.toLowerCase();
      return '[' + type + (label ? ': ' + label : '') + ']';
    }
    if (tag === 'BUTTON') {
      const t = text(el).trim();
      return t ? '[button: ' + t + ']' : '';
    }
    const children = () => Array.from(el.childNodes).map(c => render(c, ctx)).join('');
    if (/^H[1-6]$/.test(tag)) {
      const level = Number(tag[1]);
      const t = children().replace(/\s+/g, ' ').trim();
      return t ? '\n\n' + '#'.repeat(level) + ' ' + t + '\n\n' : '';
    }
    if (tag === 'P') { const t = children().trim(); return t ? '\n\n' + t + '\n\n' : ''; }
    if (tag === 'STRONG' || tag === 'B') { const t = children().trim(); return t ? '**' + t + '**' : ''; }
    if (tag === 'EM' || tag === 'I') { const t = children().trim(); return t ? '*' + t + '*' : ''; }
    if (tag === 'CODE' && !ctx.pre) { const t = text(el).trim(); return t ? '\x60' + t + '\x60' : ''; }
    if (tag === 'PRE') {
      const code = (el.textContent || '').replace(/\n$/, '');
      return '\n\n\x60\x60\x60\n' + code + '\n\x60\x60\x60\n\n';
    }
    if (tag === 'A') {
      const t = children().replace(/\s+/g, ' ').trim();
      const href = el.getAttribute('href');
      if (!t) return '';
      if (!href || href.startsWith('#') || href.startsWith('javascript:')) return t;
      return '[' + t + '](' + abs(href) + ')';
    }
    if (tag === 'UL' || tag === 'OL') {
      const depth = ctx.list || 0;
      const items = Array.from(el.children).filter(c => c.tagName === 'LI');
      const out = items.map((li, i) => {
        const inner = Array.from(li.childNodes).map(c => render(c, Object.assign({}, ctx, { list: depth + 1 }))).join('').trim().replace(/\n{3,}/g, '\n\n');
        const bullet = tag === 'OL' ? (i + 1) + '. ' : '- ';
        return '  '.repeat(depth) + bullet + inner.replace(/\n(?!\s*$)/g, '\n' + '  '.repeat(depth + 1));
      }).filter(Boolean);
      return out.length ? '\n\n' + out.join('\n') + '\n\n' : '';
    }
    if (tag === 'LI') { return children(); }
    if (tag === 'BLOCKQUOTE') {
      const t = children().trim();
      return t ? '\n\n' + t.split('\n').map(l => '> ' + l).join('\n') + '\n\n' : '';
    }
    if (tag === 'TABLE') {
      const rows = Array.from(el.querySelectorAll(':scope > thead > tr, :scope > tbody > tr, :scope > tfoot > tr, :scope > tr'));
      if (!rows.length) return '';
      // A layout table (nested tables or forms, a presentation role, ragged
      // rows without any header, or the page's wrapper) is not a table:
      // its rows become lines, cells joined with " · ", links kept.
      if (!isDataTable(el, rows)) {
        // A cell keeps its own lines (a nested layout table's rows); spaces collapse.
        const lines = rows.map(r => Array.from(r.children).filter(c => c.tagName === 'TD' || c.tagName === 'TH')
          .map(c => Array.from(c.childNodes).map(n => render(n, ctx)).join('').replace(/[ \t]+/g, ' ').replace(/\s*\n\s*/g, '\n').trim()).filter(Boolean).join(' · ')).filter(Boolean);
        return lines.length ? '\n\n' + lines.join('\n') + '\n\n' : '';
      }
      const cells = rows.map(r => Array.from(r.children).filter(c => c.tagName === 'TD' || c.tagName === 'TH').map(c => escapeCell(Array.from(c.childNodes).map(n => render(n, ctx)).join('').replace(/\s+/g, ' '))));
      const width = Math.max(...cells.map(r => r.length));
      if (!width) return '';
      const pad = r => { while (r.length < width) r.push(''); return r; };
      const header = pad(cells[0]);
      const body = cells.slice(1).map(pad);
      const lines = ['| ' + header.join(' | ') + ' |', '| ' + header.map(() => '---').join(' | ') + ' |'].concat(body.map(r => '| ' + r.join(' | ') + ' |'));
      return '\n\n' + lines.join('\n') + '\n\n';
    }
    if (tag === 'DT') { const t = children().trim(); return t ? '\n\n**' + t + '**\n' : ''; }
    if (tag === 'DD') { const t = children().trim(); return t ? ': ' + t + '\n' : ''; }
    if (tag === 'MAIN' || tag === 'ARTICLE' || el.getAttribute('role') === 'main') {
      const t = Array.from(el.childNodes).map(c => render(c, Object.assign({}, ctx, { inMain: true }))).join('');
      return '\n\n' + t + '\n\n';
    }
    const t = children();
    return BLOCK.has(tag) ? '\n' + t + '\n' : t;
  }
  // Data tables get the pipe syntax; everything else is layout.
  function isDataTable(el, rows) {
    if (el.getAttribute('role') === 'presentation' || el.getAttribute('role') === 'none') return false;
    if (el.querySelector('table, form, iframe')) return false;
    if (el.querySelector(':scope > thead, :scope > tr > th, :scope > tbody > tr > th')) return true;
    if (rows.length < 2) return false;
    const widths = rows.map(r => Array.from(r.children).filter(c => c.tagName === 'TD' || c.tagName === 'TH').length);
    const width = widths[0];
    if (width < 2 || widths.some(w => w !== width)) return false;
    // The wrapper of (nearly) the whole page is layout, whatever its shape.
    const bodyText = (document.body && document.body.textContent || '').replace(/\s+/g, ' ').length;
    const ownText = (el.textContent || '').replace(/\s+/g, ' ').length;
    return !(bodyText > 0 && ownText / bodyText > 0.85);
  }
  function tidy(md) {
    return md.replace(/[ \t]+\n/g, '\n').replace(/\n{3,}/g, '\n\n').replace(/^\s+|\s+$/g, '');
  }
  let root = document.body;
  if (opts.selector) {
    root = document.querySelector(opts.selector);
    if (!root) return { error: 'selector matched nothing' };
  } else if (opts.element) {
    root = opts.element;
  }
  const scoped = Boolean(opts.selector || opts.element);
  // Main content: <main>, role=main, <article>, else the body without nav/footer/aside.
  let main = null;
  if (!scoped) {
    main = document.querySelector('main, [role="main"]') || document.querySelector('article');
  }
  // Open dialogs first (a whole-page read; a scoped read gets what it names).
  const dialogs = scoped ? [] : findDialogs(false);
  const dialogSet = new Set(dialogs);
  const dialogMd = dialogs.map((el) => {
    const title = dialogTitle(el);
    const body = tidy(render(el, { inMain: true, pre: false, dialog: true }));
    return '## Dialog' + (title ? ': ' + title : '') + '\n\n' + body;
  }).filter(Boolean).join('\n\n');
  const withDialogs = (md) => dialogMd ? tidy(dialogMd + '\n\n---\n\n' + md) : md;
  const mainBody = tidy(render(main || root, { inMain: Boolean(main) || scoped, pre: false }));
  const mainMd = withDialogs(mainBody);
  const fullMd = main ? withDialogs(tidy(render(root, { inMain: false, pre: false }))) : mainMd;
  const fullText = (root.innerText || '').replace(/\s+/g, ' ').trim();
  const headings = root.querySelectorAll('h1, h2, h3, h4, h5, h6').length;
  const links = root.querySelectorAll('a[href]').length;
  const forms = root.querySelectorAll('form').length;
  const password = Array.from(document.querySelectorAll('input[type="password"]')).some(i => !hidden(i));
  const loginUrl = /login|signin|sign-in|auth|sso/i.test(location.href);
  return {
    markdown: mainMd,
    full_markdown: fullMd,
    main_chars: mainBody.length,
    dialogs: dialogs.length,
    text_chars: fullText.length,
    headings, links, forms,
    login_wall: password || loginUrl,
    title: document.title,
  };
})`;

// Every same-document anchor: text + absolute href, deduplicated by href.
export const LINKS_SOURCE = String.raw`(function (args) {
  const filter = (args && args.filter ? String(args.filter) : '').toLowerCase();
  const max = args && args.max ? Number(args.max) : 100;
  const seen = new Map();
  for (const a of document.querySelectorAll('a[href]')) {
    let href; try { href = new URL(a.getAttribute('href'), document.baseURI).href; } catch { continue; }
    if (href.startsWith('javascript:')) continue;
    const text = (a.innerText || a.textContent || a.getAttribute('aria-label') || a.getAttribute('title') || '').replace(/\s+/g, ' ').trim();
    if (seen.has(href)) continue;
    if (filter && !text.toLowerCase().includes(filter) && !href.toLowerCase().includes(filter)) continue;
    seen.set(href, { text: text.slice(0, 200), href });
  }
  const all = Array.from(seen.values());
  return { links: all.slice(0, max), total: all.length };
})`;

// ---------------------------------------------------------------------------
// Node-side helpers of the act (host.mjs), pure so the tests reach them
// without a browser.

const modalKey = (m) => [m.title || ''].concat(m.buttons || []).join('\u0001');

/**
 * What a click or key press changed, from MODALS_SOURCE probes taken before
 * and after it: `modal` (the first modal dialog that was not open before:
 * title + button labels), `title_before` (when the title changed) and
 * `focused` (role + name of the element that has the focus now, when it
 * moved to another element than the one acted on: `after.focused.acted`).
 * Empty when nothing changed or a probe failed.
 */
export function actChanges(before, after) {
  const out = {};
  if (!after) return out;
  const open = new Set(((before && before.modals) || []).map(modalKey));
  const fresh = (after.modals || []).filter((m) => !open.has(modalKey(m)));
  if (fresh.length) out.modal = { title: fresh[0].title || '', buttons: fresh[0].buttons || [] };
  if (before && typeof after.title === 'string' && after.title !== before.title) out.title_before = before.title;
  const f = after.focused;
  const b = before && before.focused;
  if (f && !f.acted && (!b || b.role !== f.role || b.name !== f.name)) out.focused = { role: f.role, name: f.name };
  return out;
}

const MASKED = /^‹\d+ chars›$/;
/** A Playwright message with the text an act submitted replaced by its length:
 *  the call log quotes it verbatim (`fill("…")`, `elementHandle.type("…")`).
 *  `text`, when known, is masked exactly (quotes and newlines included); any
 *  other fill/type argument is masked to the end of its line. */
export function redactActText(message, text) {
  let out = String(message || '');
  const t = text == null ? '' : String(text);
  if (t) for (const op of ['fill', 'type']) out = out.split(`${op}("${t}")`).join(`${op}("‹${[...t].length} chars›")`);
  // the rest of the line (a text with a newline has no closing `")` on it)
  return out.replace(/\b(fill|type)\("(.*?)(?:"\))?$/gm, (all, op, v) => (MASKED.test(v) ? all : `${op}("‹${[...v].length} chars›")`));
}

/** The last `n` steps of a Playwright error's call log (`  - …` lines), one
 *  line, a submitted text masked (`redactActText`). */
export function callLogTail(message, n = 2) {
  // eslint-disable-next-line no-control-regex
  const lines = redactActText(String(message || '').replace(/\x1b\[[0-9;]*m/g, '')).split('\n');
  const at = lines.findIndex((l) => /^\s*Call log:/i.test(l));
  const steps = (at >= 0 ? lines.slice(at + 1) : lines)
    .map((l) => l.trim())
    .map((l) => (l.startsWith('- ') ? l.slice(2).trim() : /^\d+ × /.test(l) ? l : ''))
    // the retry bookkeeping says nothing about the cause
    .filter((l) => l && !/^(waiting \d+ ?ms|retrying \w+ action)$/i.test(l))
    .map((l) => l.slice(0, 120));
  // the latest distinct steps (Playwright repeats its retry lines)
  const out = [];
  for (let i = steps.length - 1; i >= 0 && out.length < n; i--) if (!out.includes(steps[i])) out.unshift(steps[i]);
  return out.join(' · ');
}

/** The one-line action_timeout message: the element, the budget, forced or not, the call log's tail, what to do. */
export function actTimeoutMessage({ kind, target, role, name, timeout_ms, force, hidden, connected, message }) {
  const element = name ? `${role || 'element'} "${name}" (${target})` : `${role || 'element'} (${target})`;
  const mode = force ? 'forced (background tab: no actionability checks)' : (hidden ? 'not forced (background tab)' : 'not forced');
  // connected: false = left the page, null = the page did not answer the probe
  const where = connected === false ? `the element left the page during the ${kind}` : connected === null ? 'the page did not answer whether the element is still there' : 'the element still resolves';
  const tail = callLogTail(message);
  // the advice before the call log: the sidecar keeps the first 500 chars
  return `${kind} on ${element} did not complete within ${timeout_ms} ms; ${where}; ${mode}; take browser_screenshot to see what is in the way (a modal, an overlay, a disabled or hidden control) before retrying${tail ? `; playwright: ${tail}` : ''}`;
}
