// What a remote companion (src/remote.js) takes from the other machine, without Electron: its
// text, its URLs, the pages it restores (contract §3.2 rules 3, 9, 11; amendment A4.3), and a
// shown file served through the helper (`link.file`, §4.2). Another machine's file is never a
// path here: it is `herdr-file://<token>/<percent-encoded path below the folder it serves>`.
'use strict';

const FILE_SCHEME = 'herdr-file';
// standard + secure + fetch + stream; no service workers
const FILE_PRIVILEGES = { standard: true, secure: true, supportFetchAPI: true, stream: true };
const FILE_MAX = 32 * 1024 * 1024; // what the other machine serves at most
const FILE_PULLS = 16; // pulls on the way at once (the helper keeps no more)
const TEXT_MAX = 300;

// One line of the other machine's text for a title, the bar or a menu: no control characters,
// no bidi or invisible ones (they could reorder the machine's name beside it), clipped.
function cleanText(v, max = TEXT_MAX) {
  if (typeof v !== 'string') return '';
  return v
    .replace(/[\t\n\v\f\r\u2028\u2029]+/g, ' ')
    .replace(/[\u0000-\u001f\u007f-\u009f\u061c\u200b\u200e\u200f\u202a-\u202e\u2060-\u206f\ufeff]/g, '')
    .slice(0, max)
    .trim();
}

// A page's URL: http(s) without control characters, or exactly about:blank.
function isWebUrl(url) {
  return typeof url === 'string' && (url === 'about:blank' || (/^https?:\/\//i.test(url) && !/[\u0000-\u001f\u007f-\u009f]/.test(url)));
}

function isFileToken(token) {
  return typeof token === 'string' && /^[a-z][a-z0-9]{23}$/.test(token);
}

// The gate's rule (rel_path in the link's ops.json), as the sidecar has it: no empty or dot
// segment (`%2e` is a dot to a URL), no encoded separator or NUL, no query or fragment.
function isFileRel(rel) {
  return (
    typeof rel === 'string' &&
    !/[\u0000-\u001f\u007f\\?#]|%2f|%5c|%00/i.test(rel) &&
    rel.split('/').every((part) => !/^\.{0,2}$/.test(part.replace(/%2e/gi, '.')))
  );
}

const fileUrl = (token, rel) => `${FILE_SCHEME}://${token}/${rel}`;

// {token, rel, suffix: its query and fragment}, or null. `rel` is not checked here (isFileRel).
function parseFileUrl(url) {
  const m = /^herdr-file:\/\/([a-z][a-z0-9]{23})\/([^?#]*)([?#][^]*)?$/.exec(String(url || ''));
  return m ? { token: m[1], rel: m[2], suffix: m[3] || '' } : null;
}

// The same page under another token (a URL that is not `from`'s stays as it is).
function retoken(url, from, to) {
  const f = parseFileUrl(url);
  return f && f.token === from && isFileToken(to) ? fileUrl(to, f.rel) + f.suffix : url;
}

// A host_state entry: {url, serve} a companion may keep, or null (dropped). A shown file needs
// its own serve token; every other URL that is no web page becomes about:blank.
function remoteEntry(e) {
  const url = e && typeof e.url === 'string' ? e.url : '';
  if (!e || e.kind !== 'html_view') return e ? { url: isWebUrl(url) ? url : 'about:blank', serve: null } : null;
  if (!isFileToken(e.serve)) return null;
  const f = parseFileUrl(url);
  const own = !!f && f.token === e.serve && isFileRel(f.rel);
  return { url: own || isWebUrl(url) ? url : 'about:blank', serve: e.serve };
}

// The lease's `link` (the helper's, on register and heartbeat answers): {state, text} or null
// when it says nothing a companion knows.
const LINK_STATES = new Set(['linked', 'reconnecting', 'in_use']);
function linkState(link) {
  if (!link || !LINK_STATES.has(link.state)) return null;
  return { state: link.state, text: cleanText(link.text, 200) || null };
}

// ---------- a shown file, pulled through the helper ----------

// At most `max` at once, the rest in order.
class Lanes {
  constructor(max = FILE_PULLS) {
    this.free = max;
    this.waiting = [];
  }

  async run(fn) {
    if (this.free > 0) this.free -= 1;
    else await new Promise((resolve) => this.waiting.push(resolve)); // takes over the place
    try {
      return await fn();
    } finally {
      const next = this.waiting.shift();
      if (next) next();
      else this.free += 1;
    }
  }
}

// A `link.file` answer, checked: {status, mime, size, data: Buffer, eof}. Throws on nonsense.
function fileChunk(r) {
  const status = r && r.status;
  if (!Number.isInteger(status) || status < 200 || status > 599) throw new Error('bad answer');
  if (status !== 200) return { status: status >= 400 ? status : 502 };
  if (!Number.isInteger(r.size) || r.size < 0 || r.size > FILE_MAX || typeof r.data !== 'string') throw new Error('bad answer');
  const mime = typeof r.mime === 'string' && /^[\x21-\x7e][\x20-\x7e]{0,199}$/.test(r.mime) ? r.mime : 'application/octet-stream';
  return { status, mime, size: r.size, data: Buffer.from(r.data, 'base64'), eof: r.eof === true };
}

// Answers a page's request in the partition of `token`. `pull(rel, offset)` is a `link.file`
// call's answer (rejects while the link is down: 502). `down`: the 502 page's text.
async function fileResponse(request, { token, pull, down = 'the connection is down' }) {
  const plain = (status, text) => new Response(text, { status, headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' } });
  if (request.method !== 'GET' && request.method !== 'HEAD') return plain(405, 'not allowed');
  const f = parseFileUrl(request.url);
  if (!f || f.token !== token) return plain(403, 'another page\'s file');
  if (!isFileRel(f.rel)) return plain(404, 'not found');
  let first;
  try {
    first = fileChunk(await pull(f.rel, 0));
  } catch {
    return plain(502, down);
  }
  if (first.status !== 200) return plain(first.status, first.status === 404 ? 'not found' : first.status === 403 ? 'not allowed' : down);
  const { size } = first;
  let offset = 0;
  // false: the file is through
  const take = (chunk, c) => {
    if (chunk.status !== 200 || chunk.size !== size || offset + chunk.data.length > size || (!chunk.data.length && !chunk.eof)) throw new Error('the file changed or broke off');
    offset += chunk.data.length;
    if (chunk.eof && offset !== size) throw new Error('the file broke off');
    if (chunk.data.length) c.enqueue(new Uint8Array(chunk.data));
    if (chunk.eof) c.close();
  };
  let body;
  try {
    body = new ReadableStream({
      start(c) {
        take(first, c);
      },
      async pull(c) {
        take(fileChunk(await pull(f.rel, offset)), c);
      },
    });
  } catch {
    return plain(502, down); // its first chunk is nonsense
  }
  const headers = { 'content-type': first.mime, 'content-length': String(size), 'cache-control': 'no-store' };
  return new Response(request.method === 'HEAD' ? null : body, { status: 200, headers });
}

module.exports = {
  FILE_SCHEME,
  FILE_PRIVILEGES,
  FILE_MAX,
  FILE_PULLS,
  cleanText,
  isWebUrl,
  isFileToken,
  isFileRel,
  fileUrl,
  parseFileUrl,
  retoken,
  remoteEntry,
  linkState,
  Lanes,
  fileChunk,
  fileResponse,
};
