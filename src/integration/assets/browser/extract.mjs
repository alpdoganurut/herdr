// herdr browser: main-content → markdown, run inside the page with page.evaluate.
// No dependencies (no Readability, no Turndown): headings, paragraphs, lists,
// pipe tables, links as [text](href), images as ![alt], code, quotes; drops
// nav/footer/aside/script/style/template/noscript, hidden and aria-hidden nodes.

export const EXTRACT_SOURCE = String.raw`(function (args) {
  const opts = args || {};
  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'CANVAS', 'IFRAME', 'OBJECT', 'EMBED', 'VIDEO', 'AUDIO', 'MAP', 'DIALOG']);
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
  const mainMd = tidy(render(main || root, { inMain: Boolean(main) || scoped, pre: false }));
  const fullMd = main ? tidy(render(root, { inMain: false, pre: false })) : mainMd;
  const fullText = (root.innerText || '').replace(/\s+/g, ' ').trim();
  const headings = root.querySelectorAll('h1, h2, h3, h4, h5, h6').length;
  const links = root.querySelectorAll('a[href]').length;
  const forms = root.querySelectorAll('form').length;
  const password = Array.from(document.querySelectorAll('input[type="password"]')).some(i => !hidden(i));
  const loginUrl = /login|signin|sign-in|auth|sso/i.test(location.href);
  return {
    markdown: mainMd,
    full_markdown: fullMd,
    main_chars: mainMd.length,
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
