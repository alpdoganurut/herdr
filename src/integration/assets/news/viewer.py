#!/usr/bin/env python3
"""herdr AI news viewer: the page set like a leaf from an old almanac.

One idea: marginalia. At 100+ columns the source and time of every story are
set as side notes in the outer margin, aligned with the headline; at 160+ the
page opens into a two-page spread with a gutter, notes in the outer margins of
each leaf. Below 100 columns the notes fall inline under each story.

Everything else is quiet: a title page with fleurons, a rust drop cap on the
lead, justified body text, ornamental section rules, a manicule (☞) pointing at
the selected story, and a folio line at the bottom (theme, edition, leaf).

Editions: editions/index.json beside page.json. [ ] step days, { } step runs, l latest.

Usage: python3 almanac.py page.json            interactive
       python3 almanac.py page.json --dump W   print the page at width W

Keys: j/k or arrows scroll, space/b leaf forward/back, g/G top/bottom,
tab/shift-tab select a story, enter opens it, r reload, q quit.
"""
import json, os, re, select, signal, subprocess, sys, termios, time, tty, unicodedata
from datetime import datetime

# ------------------------------------------------------------------ arguments
def parse_args(argv):
    path, dump = None, None
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--edition":
            i += 1
            if i < len(argv): os.environ["ALMANAC_EDITION"] = argv[i]
        elif a == "--theme":
            i += 1
            if i < len(argv): os.environ["ALMANAC_THEME"] = argv[i]
        elif a == "--dump":
            i += 1
            dump = int(argv[i]) if i < len(argv) else 80
        elif path is None:
            path = a
        i += 1
    return path or "page.json", dump

# ------------------------------------------------------------------ palette
def rgb(h): return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))
# Soft dark themes: lifted page grounds, ink kept below full white, muted accents.
# keys: page (background), ink (headlines), body, note (marginalia), dim (chrome),
#       rule, gold (ornaments, section titles), rust (drop cap, manicule), sel (selection ground)
THEMES = {
    "Sepia": dict(page="#1c1915", ink="#e0d4bb", body="#bcae92", note="#9a8b6e", dim="#766a55",
                  rule="#463d2f", gold="#c19a5b", rust="#b8674a", sel="#2a241c"),
    "Ash":   dict(page="#1b1a19", ink="#d6d1c8", body="#aea9a0", note="#8f8a82", dim="#6f6b65",
                  rule="#3b3936", gold="#b0a58e", rust="#bf8a70", sel="#282624"),
    "Moss":  dict(page="#181b17", ink="#d0d6c6", body="#a9b09f", note="#8a9282", dim="#6d7467",
                  rule="#384035", gold="#a3b083", rust="#c9a06a", sel="#232822"),
    "Dusk":  dict(page="#17181e", ink="#cfd2de", body="#a6aabb", note="#888c9d", dim="#6b6f80",
                  rule="#363947", gold="#a5a9cf", rust="#d09a8a", sel="#22242e"),
    "Plum":  dict(page="#1b171b", ink="#dcd0d6", body="#b3a7ae", note="#93878e", dim="#75696f",
                  rule="#40363d", gold="#bf9fb3", rust="#d0a676", sel="#291f27"),
}
THEME_ORDER = ["Dusk"] + [t for t in THEMES if t != "Dusk"]
THEME_FILE = [None]  # set in main(): lives beside page.json, not beside this script
C = {}
THEME = [None]

def set_theme(name):
    name = name if name in THEMES else THEME_ORDER[0]
    C.clear(); C.update({k: rgb(v) for k, v in THEMES[name].items()})
    THEME[0] = name

def saved_theme():
    try:
        with open(THEME_FILE[0]) as f: return f.read().strip()
    except OSError: return None

def save_theme(name):
    try:
        with open(THEME_FILE[0], "w") as f: f.write(name)
    except OSError: pass

def sgr(fg=None, bold=False, italic=False, bg=None):
    p = ["0"]
    if bold: p.append("1")
    if italic: p.append("3")
    if fg: p.append("38;2;%d;%d;%d" % C[fg])
    if bg: p.append("48;2;%d;%d;%d" % C[bg])
    elif "page" in C: p.append("48;2;%d;%d;%d" % C["page"])
    return "\x1b[" + ";".join(p) + "m"


# ------------------------------------------------------------------ cells
def width(s):
    return sum(0 if unicodedata.combining(ch) else (2 if unicodedata.east_asian_width(ch) in "WF" else 1) for ch in s)

def cut_cells(s, w):
    out, used = [], 0
    for ch in s:
        cw = width(ch)
        if used + cw > w: break
        out.append(ch); used += cw
    return "".join(out)

# A row is a list of segments (text, style, url).
def seg(t, st=None, url=None): return (t, sgr() if st is None else st, url)
def row_width(r): return sum(width(t) for t, _, _ in r)
def pad(r, w):
    d = w - row_width(r)
    return r + [seg(" " * d)] if d > 0 else list(r)
def center(r, w):
    d = w - row_width(r)
    if d <= 0: return list(r)
    l = d // 2
    return [seg(" " * l)] + list(r) + [seg(" " * (d - l))]
def rjust(r, w):
    d = w - row_width(r)
    return [seg(" " * d)] + list(r) if d > 0 else list(r)

# ------------------------------------------------------------------ type
SMALL = dict(zip("abcdefghijklmnopqrstuvwxyz", "ᴀʙᴄᴅᴇꜰɢʜɪᴊᴋʟᴍɴᴏᴘǫʀꜱᴛᴜᴠᴡxʏᴢ"))
def smallcaps(s): return "".join(SMALL.get(ch, ch) for ch in s.lower())
def spaced(s): return " ".join(s.upper())

def roman(n):
    out = ""
    for v, s in ((1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
                 (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")):
        while n >= v: out += s; n -= v
    return out or "0"

def ordinal(d):
    return "%d%s" % (d, "th" if 11 <= d % 100 <= 13 else {1: "st", 2: "nd", 3: "rd"}.get(d % 10, "th"))

def local(iso):
    try: return datetime.fromisoformat(iso).astimezone()
    except Exception: return None

def fmt_time(iso):
    d = local(iso)
    return d.strftime("%H:%M") if d else ""

# 3-row half-block capitals for the drop cap.
FONT = {
    "A": ["▄▀▀▄", "█▀▀█", "▀  ▀"], "B": ["█▀▀▄", "█▀▀▄", "▀▀▀ "], "C": ["▄▀▀▀", "█   ", "▀▀▀▀"],
    "D": ["█▀▀▄", "█  █", "▀▀▀ "], "E": ["█▀▀▀", "█▀▀ ", "▀▀▀▀"], "F": ["█▀▀▀", "█▀▀ ", "▀   "],
    "G": ["▄▀▀▀", "█ ▀█", "▀▀▀▀"], "H": ["█  █", "█▀▀█", "▀  ▀"], "I": ["▀█▀", " █ ", "▀▀▀"],
    "J": [" ▀█▀", "  █ ", "▀▀  "], "K": ["█ ▄▀", "█▀▄ ", "▀  ▀"], "L": ["█   ", "█   ", "▀▀▀▀"],
    "M": ["█▄ ▄█", "█ ▀ █", "▀   ▀"], "N": ["█▄  █", "█ ▀▄█", "▀   ▀"], "O": ["▄▀▀▄", "█  █", "▀▀▀▀"],
    "P": ["█▀▀▄", "█▀▀▀", "▀   "], "Q": ["▄▀▀▄", "█  █", "▀▀▀▄"], "R": ["█▀▀▄", "█▀▀▄", "▀  ▀"],
    "S": ["▄▀▀▀", "▀▀▀▄", "▀▀▀ "], "T": ["▀█▀", " █ ", " ▀ "], "U": ["█  █", "█  █", "▀▀▀▀"],
    "V": ["█   █", "▀▄ ▄▀", "  ▀  "], "W": ["█   █", "█ ▄ █", "▀▀▀▀▀"], "X": ["▀▄ ▄▀", " ▄▀▄ ", "▀   ▀"],
    "Y": ["▀▄ ▄▀", "  █  ", "  ▀  "], "Z": ["▀▀▀█", " ▄▀ ", "▀▀▀▀"],
}

# ------------------------------------------------------------------ paragraphs
def take_line(words, w):
    """Greedy: pop words from the front that fit in w cells. Hard-cuts an overlong word."""
    line, used = [], 0
    while words:
        wd = words[0]; ww = width(wd)
        need = ww + (1 if line else 0)
        if used + need > w:
            if not line:
                cut = cut_cells(wd, w) or wd[:1]
                line.append(cut); rest = wd[len(cut):]
                if rest: words[0] = rest
                else: words.pop(0)
            break
        line.append(wd); used += need; words.pop(0)
    return line

def justify_line(words, w, last):
    """Full justification, restrained: at most one extra space per gap, never the last line."""
    s = " ".join(words)
    gaps = len(words) - 1
    extra = w - width(s)
    if last or gaps < 1 or extra <= 0 or extra > max(1, gaps // 2): return s
    at = {int((k + 0.5) * gaps / extra) for k in range(extra)}
    out = words[0]
    for i, wd in enumerate(words[1:]):
        out += "  " + wd if i in at else " " + wd
    return out

def paragraph(text, w, first=()):
    """Wrap + justify. `first` gives widths for the leading lines (drop cap indent)."""
    words = text.split()
    lines, i = [], 0
    while words:
        lw = first[i] if i < len(first) else w
        lines.append((take_line(words, lw), lw)); i += 1
    return [justify_line(ws, lw, n == len(lines) - 1) for n, (ws, lw) in enumerate(lines)] or [""]

def wrap_plain(text, w):
    words = text.split(); out = []
    while words: out.append(" ".join(take_line(words, w)))
    return out or [""]

# ------------------------------------------------------------------ geometry
def geometry(W):
    gut = 2                                   # manicule gutter beside the text
    if W >= 160:
        G = 7                                 # binding gutter between the leaves
        P = (W - 2 - G) // 2
        MW = 16
        T = min(72, P - gut - 3 - MW)
        P = gut + T + 3 + MW
        g = dict(mode="spread", T=T, MW=MW, P=P, G=G, total=2 * P + G)
    elif W >= 100:
        MW = 16
        T = min(72, W - 2 - gut - 3 - MW)
        g = dict(mode="margin", T=T, MW=MW, P=gut + T + 3 + MW, G=0, total=gut + T + 3 + MW)
    else:
        T = max(20, min(76, W - 2 - gut))
        g = dict(mode="inline", T=T, MW=0, P=gut + T, G=0, total=gut + T)
    g["gut"] = gut
    g["left"] = max(0, (W - g["total"]) // 2)
    return g

# ------------------------------------------------------------------ blocks
# A block: rows = [(gutter_text, text_segs, note_lines)], heads = [(story_idx, row_offset)],
# sec = section index (None title, -1 lead), keep = keep with next block.
def block(rows, heads=(), sec=None, keep=False):
    return dict(rows=rows, heads=list(heads), sec=sec, keep=keep)

def story_block(item, g, sel, idx, lead=False, sec=-1):
    T, MW, mode = g["T"], g["MW"], g["mode"]
    rows, heads = [], []
    selected = sel == idx
    hs = sgr("gold", bold=True, bg="sel") if selected else sgr("ink", bold=True)
    head = item.get("head", "")
    head_lines = [spaced(head)] if lead and width(spaced(head)) <= T else wrap_plain(head, T)
    for n, line in enumerate(head_lines):
        gutter = "☞ " if (selected and n == 0) else "  "
        if n == 0: heads.append((idx, len(rows)))
        rows.append((gutter, [seg(line, hs, item.get("url"))], None))
    body = (item.get("standfirst") or item.get("text", "")) if lead else item.get("text", "")
    body = " ".join(body.split())
    cap = body[:1].upper() if lead else ""
    if cap in FONT and len(body) > 1 and T >= 32:
        glyph = FONT[cap]; gw = len(glyph[0])
        lines = paragraph(body[1:].lstrip(), T, first=[T - gw - 1] * 3)
        while len(lines) < 3: lines.append("")
        for n, line in enumerate(lines):
            if n < 3:
                rows.append(("  ", [seg(glyph[n], sgr("rust")), seg(" "), seg(line, sgr("body"))], None))
            else:
                rows.append(("  ", [seg(line, sgr("body"))], None))
    elif body:
        for line in paragraph(body, T):
            rows.append(("  ", [seg(line, sgr("body"))], None))
    src, t = item.get("source", "").strip(), fmt_time(item.get("time", ""))
    if mode == "inline":
        meta = src + (", " + t if src and t else t)
        if meta: rows.append(("  ", [seg("— " + meta, sgr("dim", italic=True))], None))
    else:
        notes = [[seg(l, sgr("note", italic=True))] for l in wrap_plain(src, MW)] if src else []
        if t: notes.append([seg(t, sgr("dim"))])
        while len(rows) < len(notes): rows.append(("  ", [], None))
        if notes:
            r0 = rows[0]
            rows[0] = (r0[0], r0[1], notes)
    rows.append(("  ", [], None))                        # breathing room after the story
    return block(rows, heads, sec=sec)

def section_block(title, n, g, first):
    T = g["T"]
    label = spaced(title)
    if width(label) + 10 > T: label = title.upper()
    mid = [seg("  ✦  ", sgr("gold")), seg(label, sgr("gold")), seg("  ✦  ", sgr("gold"))]
    fill = T - row_width(mid)
    l = max(0, fill // 2); r = max(0, fill - l)
    rule = [seg("─" * l, sgr("rule"))] + mid + [seg("─" * r, sgr("rule"))]
    rows = [] if first else [("  ", [], None)]
    rows += [("  ", rule, None), ("  ", [], None)]
    return block(rows, sec=n, keep=True)

# ------------------------------------------------------------------ title page and colophon
def title_rows(page, tw):
    """Full-width rows for the title page. Returns (rows, secs, ornament_row_index)."""
    rows = []
    def add(r): rows.append(center(r, tw))
    ed = page.get("edition", 1)
    upd = local(page.get("updated", ""))
    add([])
    add([seg("❦", sgr("gold"))])
    add([])
    add([seg(spaced("The AI Almanac"), sgr("ink", bold=True))])
    add([])
    ed_s = "Edition " + roman(ed) + ("  ·  Proof impression" if page.get("stub") else "")
    add([seg("✦   ", sgr("gold")), seg(ed_s, sgr("body")), seg("   ✦", sgr("gold"))])
    if upd:
        date_s = "%s, the %s of %s %d  ·  %s" % (upd.strftime("%A"), ordinal(upd.day), upd.strftime("%B"), upd.year, upd.strftime("%H:%M"))
    else:
        date_s = page.get("updated", "")
    for l in wrap_plain(date_s, tw): add([seg(l, sgr("dim"))])
    add([])
    orn = len(rows)
    add(ornament(tw, 1.0))
    add([])
    since = [s for s in (page.get("since_last_run") or []) if s]
    if since:
        nw = min(tw, 64)
        text = "To the reader:  " + "  ".join(since)
        lines = wrap_plain(text, nw)
        for i, l in enumerate(lines):
            if i == 0:
                lab = "To the reader:"
                r = [seg(lab, sgr("gold", italic=True)), seg(l[len(lab):], sgr("body", italic=True))]
            else:
                r = [seg(l, sgr("body", italic=True))]
            rows.append(center(pad(r, nw), tw))
        add([])
    return rows, orn

def ornament(tw, frac):
    """The title rule, drawn outward from its fleuron by `frac` (for the intro)."""
    half = min(tw, 60) // 2 - 3
    n = int(round(half * frac))
    return [seg("─" * n, sgr("rule")), seg("  ❧  ", sgr("gold")), seg("─" * n, sgr("rule"))]

def sparkline(page):
    hours = []
    items = ([page["lead"]] if page.get("lead") else []) + [it for s in page.get("sections", []) for it in s.get("items", [])]
    for it in items:
        d = local(it.get("time", ""))
        if d: hours.append(d.hour)
    if len(set(hours)) < 2: return ""
    lo, hi = min(hours), max(hours)
    counts = [hours.count(h) for h in range(lo, hi + 1)]
    m = max(counts); bars = "▁▂▃▄▅▆▇█"
    return "".join(bars[0] if c == 0 else bars[min(7, 1 + (c - 1) * 7 // max(1, m))] for c in counts) + "  %02d–%02dh" % (lo, hi)

def colophon_rows(page, tw):
    rows = []
    def add(r): rows.append(center(r, tw))
    st = page.get("stats", {})
    add([])
    add([seg("⁂", sgr("gold"))])
    add([])
    line = "%s items read from %s sources" % (st.get("anchor_items", "?"), st.get("sources", "?"))
    sp = sparkline(page)
    if sp and width(line) + 5 + width(sp) <= tw:
        add([seg(line, sgr("dim")), seg("  ·  ", sgr("rule")), seg(sp, sgr("dim"))])
    else:
        for l in wrap_plain(line, tw): add([seg(l, sgr("dim"))])
        if sp: add([seg(sp, sgr("dim"))])
    nr = page.get("next_run")
    add([seg("Next impression " + (fmt_time(nr) if nr else "not scheduled"), sgr("dim", italic=True))])
    add([])
    return rows

# ------------------------------------------------------------------ placing blocks on a leaf
def place(blocks, g, side):
    """Lay blocks down a leaf. side: 'inline' | 'right' (notes in the right margin) |
    'left' (notes in the left margin, rows exactly P wide). Returns (rows, secs, targets)."""
    T, MW, P = g["T"], g["MW"], g["P"]
    out, secs, targets = [], [], []
    started = False
    for b in blocks:
        rows = b["rows"]
        skipped = 0
        if not started:
            while skipped < len(rows) and not rows[skipped][1] and not rows[skipped][2]: skipped += 1
            rows = rows[skipped:]
        started = started or bool(rows)
        base = len(out)
        # spread notes down from their anchor row
        note_at = {}
        for k, (_, _, notes) in enumerate(rows):
            if notes:
                for j, nl in enumerate(notes): note_at[k + j] = nl
        for k, (gutter, text, _) in enumerate(rows):
            gs = seg(gutter, sgr("rust"))
            nl = note_at.get(k)
            if side == "inline":
                out.append([gs] + text)
            elif side == "right":
                r = [gs] + pad(text, T)
                if nl: r += [seg("   ")] + nl
                out.append(r)
            else:
                n = rjust(nl, MW) if nl else [seg(" " * MW)]
                out.append(n + [seg("   "), gs] + pad(text, T))
            secs.append(b["sec"])
        for i, off in b["heads"]:
            targets.append((i, base + off - skipped))
    return out, secs, targets

def split_blocks(blocks):
    """Split the block stream near the middle for a two-leaf spread, never after a section opener."""
    total = sum(len(b["rows"]) for b in blocks)
    best, best_d, cum = len(blocks), None, 0
    for i in range(1, len(blocks)):
        cum += len(blocks[i - 1]["rows"])
        if blocks[i - 1]["keep"]: continue
        d = abs(cum - total / 2)
        if best_d is None or d < best_d: best, best_d = i, d
    return blocks[:best], blocks[best:]

# ------------------------------------------------------------------ the page
def build(page, W, sel):
    """Return (rows, secs, targets, ornament_row). targets = [(story_idx, row, url)]."""
    g = geometry(W)
    L = g["left"]; tw = g["total"]
    rows, secs = [], []
    def add_full(r, s=None):
        rows.append([seg(" " * L)] + r if L else list(r)); secs.append(s)

    trows, orn = title_rows(page, tw)
    for r in trows: add_full(r)
    orn_row = orn

    # story blocks in reading order
    blocks, urls = [], []
    idx = 0
    if page.get("lead"):
        blocks.append(story_block(page["lead"], g, sel, idx, lead=True, sec=-1)); urls.append(page["lead"].get("url")); idx += 1
    for n, sec in enumerate(page.get("sections", [])):
        blocks.append(section_block(sec.get("title", ""), n, g, first=not blocks))
        for it in sec.get("items", []):
            blocks.append(story_block(it, g, sel, idx, sec=n)); urls.append(it.get("url")); idx += 1

    targets = []
    top = len(rows)
    if g["mode"] == "spread":
        lb, rb = split_blocks(blocks)
        lrows, lsecs, lt = place(lb, g, "left")
        rrows, rsecs, rt = place(rb, g, "right")
        P, G = g["P"], g["G"]
        fold = [seg(" " * (G // 2)), seg("┆", sgr("rule")), seg(" " * (G - G // 2 - 1))]
        H = max(len(lrows), len(rrows))
        for y in range(H):
            lr = lrows[y] if y < len(lrows) else [seg(" " * P)]
            rr = rrows[y] if y < len(rrows) else []
            s = lsecs[y] if y < len(lsecs) else (rsecs[y] if y < len(rsecs) else None)
            add_full(lr + fold + rr, s)
        targets += [(i, top + y) for i, y in lt] + [(i, top + y) for i, y in rt]
    else:
        prows, psecs, pt = place(blocks, g, "right" if g["mode"] == "margin" else "inline")
        for r, s in zip(prows, psecs): add_full(r, s)
        targets += [(i, top + y) for i, y in pt]

    for r in colophon_rows(page, tw): add_full(r)
    targets = sorted((i, y, urls[i]) for i, y in targets)
    return rows, secs, targets, orn_row

# ------------------------------------------------------------------ terminal output
def render_row(r, W):
    out, used = [], 0
    for t, st, url in r:
        if used >= W: break
        if used + width(t) > W: t = cut_cells(t, W - used)
        if url: out.append("\x1b]8;;%s\x1b\\%s%s\x1b]8;;\x1b\\" % (url, st, t))
        else: out.append(st + t)
        used += width(t)
    out.append(sgr())
    return "".join(out)

def dump(page, W):
    rows, _, _, _ = build(page, W, -1)
    sys.stdout.write("\n".join(render_row(r, W) for r in rows) + "\n")
    sys.stdout.flush()

def section_name(page, s):
    if s is None: return ""
    if s == -1: return "The lead"
    secs = page.get("sections", [])
    return "%s  ·  %s" % (roman(s + 1), secs[s].get("title", "")) if 0 <= s < len(secs) else ""

def chrome(page, off, H, W, nrows, secs):
    """Running head on row 1, folio + keys on the last row."""
    s = next((secs[y] for y in range(off, min(len(secs), off + H)) if secs[y] is not None), None)
    left = smallcaps("The AI Almanac")
    right = section_name(page, s)
    gap = W - width(left) - width(right) - 4
    if gap < 1: right = ""; gap = max(0, W - width(left) - 4)
    head = "\x1b[H" + sgr() + "\x1b[2K" + sgr("note") + "  " + left + " " * gap + sgr("dim", italic=True) + right + sgr()
    leaves = max(1, -(-max(1, nrows) // max(1, H)))
    leaf = min(leaves, off // max(1, H) + 1)
    hl = history_label()
    folio = "%s  ·  %sleaf %d of %d  " % (THEME[0], (hl + "  ·  ") if hl else "", leaf, leaves)
    keys = "j/k scroll · tab select · enter open · [ ] day · { } run · t theme · q quit" if W >= 96 else "j/k · tab · enter · [ ] { } · t · q"
    keys = cut_cells("  " + keys, max(0, W - width(folio) - 1))
    foot = "\x1b[%d;1H" % (H + 1) + sgr() + "\x1b[2K" + sgr("dim") + keys + " " * max(0, W - width(keys) - width(folio)) + folio + sgr()
    return head, foot

def emit(page, rows, secs, off, H, W):
    out = ["\x1b[H"]                           # content starts at row 1; only the folio row is fixed
    for y in range(H):
        out.append(sgr() + "\x1b[2K")
        i = off + y
        r = rows[i] if i < len(rows) else []
        out.append(render_row(r, W))
        out.append("\r\n")
    head, foot = chrome(page, off, H, W, len(rows), secs)
    sys.stdout.write("".join(out) + foot); sys.stdout.flush()

def tokens(buf):
    """Split a raw read into key tokens (CSI sequences kept whole)."""
    out, i = [], 0
    while i < len(buf):
        if buf[i] == "\x1b" and i + 1 < len(buf) and buf[i + 1] in "[O":
            j = i + 2
            while j < len(buf) and not ("@" <= buf[j] <= "~"): j += 1
            out.append(buf[i:j + 1]); i = j + 1
        else:
            out.append(buf[i]); i += 1
    return out

def load(path):
    with open(path) as f: return json.load(f)

# ------------------------------------------------------------------ edition history
HIST = {"dir": None, "eds": [], "cur": -1}

def load_history(path):
    """editions/index.json beside page.json -> list of editions, oldest first."""
    d = os.path.join(os.path.dirname(os.path.abspath(path)), "editions")
    idx = os.path.join(d, "index.json")
    if not os.path.exists(idx): return None, []
    try:
        with open(idx) as f: return d, json.load(f).get("editions", [])
    except (OSError, json.JSONDecodeError): return d, []

def edition_page(i):
    e = HIST["eds"][i]
    with open(os.path.join(HIST["dir"], e["path"])) as f: return json.load(f)

def step_day(i, delta):
    """Last edition of the previous/next day that has editions."""
    eds = HIST["eds"]; days = sorted({e["day"] for e in eds})
    k = days.index(eds[i]["day"]) + delta
    if k >= len(days): k = len(days) - 1          # past the last day: that day's latest edition
    if k < 0: return i
    return max(j for j, e in enumerate(eds) if e["day"] == days[k])

def history_label():
    eds, i = HIST["eds"], HIST["cur"]
    if not eds or i < 0: return ""
    e = eds[i]
    try: at = datetime.fromisoformat(e["at"]).strftime("%a %-d %b %H:%M")
    except Exception: at = e.get("at", "")
    tag = "" if i == len(eds) - 1 else "  (l: latest)"
    return "%s  ·  %s of %s%s" % (at, roman(e["edition"]), roman(eds[-1]["edition"]), tag)

def main():
    path, dump_w = parse_args(sys.argv[1:])
    THEME_FILE[0] = os.path.join(os.path.dirname(os.path.abspath(path)), "viewer-theme")
    set_theme(os.environ.get("ALMANAC_THEME") or saved_theme() or THEME_ORDER[0])
    try: sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception: pass
    HIST["dir"], HIST["eds"] = load_history(path)
    if HIST["eds"]:
        want = os.environ.get("ALMANAC_EDITION")
        HIST["cur"] = next((i for i, e in enumerate(HIST["eds"]) if str(e["edition"]) == want), len(HIST["eds"]) - 1)
        page = edition_page(HIST["cur"])
    else:
        page = load(path)
    if dump_w is not None:
        dump(page, max(20, dump_w)); return

    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    rp, wp = os.pipe(); os.set_blocking(wp, False)
    signal.set_wakeup_fd(wp)
    signal.signal(signal.SIGWINCH, lambda *_: None)
    tty.setcbreak(fd)
    sys.stdout.write("\x1b[?1049h\x1b[?25l"); sys.stdout.flush()
    watch = os.path.join(HIST["dir"], "index.json") if HIST["eds"] else path
    mtime = os.path.getmtime(watch)
    off, sel, intro = 0, -1, True
    try:
        while True:
            W, Hfull = os.get_terminal_size()
            H = max(1, Hfull - 1)
            rows, secs, targets, orn = build(page, W, sel)
            maxoff = max(0, len(rows) - H)
            off = max(0, min(off, maxoff))
            emit(page, rows, secs, off, H, W)
            if intro and off <= orn < off + H:
                # one short intro moment: the title rule draws outward from its fleuron.
                # Only that one row is rewritten per frame; the last frame equals rows[orn].
                g = geometry(W); L = g["left"]; tw = g["total"]
                term_row = orn - off + 1
                for k in range(0, 7):
                    r = [seg(" " * L)] + center(ornament(tw, k / 6), tw)
                    sys.stdout.write("\x1b[%d;1H%s\x1b[2K%s" % (term_row, sgr(), render_row(r, W))); sys.stdout.flush()
                    if k < 6: time.sleep(0.035)
            intro = False
            ready, _, _ = select.select([fd, rp], [], [], 2.0)
            if rp in ready:
                try: os.read(rp, 64)
                except OSError: pass
            try:
                m = os.path.getmtime(watch)
                if m != mtime:
                    mtime = m
                    if HIST["eds"]:
                        at_latest = HIST["cur"] == len(HIST["eds"]) - 1
                        HIST["dir"], eds = load_history(path)
                        if eds: HIST["eds"] = eds
                        if at_latest: HIST["cur"] = len(HIST["eds"]) - 1
                        page = edition_page(HIST["cur"])
                    else:
                        page = load(path)
                    sel = min(sel, len(targets) - 1)
            except (OSError, json.JSONDecodeError): pass
            if fd not in ready: continue
            buf = os.read(fd, 64).decode(errors="ignore")
            quit_ = False
            for k in tokens(buf):
                if k == "q": quit_ = True; break
                elif k in ("j", "\x1b[B", "\x1bOB"): off += 1
                elif k in ("k", "\x1b[A", "\x1bOA"): off -= 1
                elif k in (" ", "\x1b[6~"): off += max(1, H - 2)
                elif k in ("b", "\x1b[5~"): off -= max(1, H - 2)
                elif k == "t":
                    i = THEME_ORDER.index(THEME[0])
                    set_theme(THEME_ORDER[(i + 1) % len(THEME_ORDER)]); save_theme(THEME[0])
                elif k in ("[", "]", "{", "}", "l") and HIST["eds"]:
                    i = HIST["cur"]
                    if k == "[": j = step_day(i, -1)
                    elif k == "]": j = step_day(i, 1)
                    elif k == "{": j = max(0, i - 1)
                    elif k == "}": j = min(len(HIST["eds"]) - 1, i + 1)
                    else: j = len(HIST["eds"]) - 1
                    if j != i:
                        try: page = edition_page(j); HIST["cur"] = j; off, sel = 0, -1
                        except (OSError, json.JSONDecodeError): pass
                elif k == "g": off = 0
                elif k == "G": off = maxoff
                elif k == "r":
                    try: page, mtime = load(path), os.path.getmtime(path)
                    except (OSError, json.JSONDecodeError): pass
                elif k in ("\t", "\x1b[Z") and targets:
                    n = len(targets)
                    sel = (sel + (1 if k == "\t" else -1)) % n if sel >= 0 else (0 if k == "\t" else n - 1)
                    row = next(y for i, y, _ in targets if i == sel)
                    if row < off or row >= off + H - 1: off = max(0, row - H // 3)
                elif k in ("\r", "\n") and sel >= 0:
                    url = next((u for i, _, u in targets if i == sel), None)
                    if url: subprocess.Popen(["open", url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                off = max(0, min(off, maxoff))
            if quit_: break
    finally:
        sys.stdout.write("\x1b[0m\x1b[?25h\x1b[?1049l"); sys.stdout.flush()
        try: signal.set_wakeup_fd(-1)
        except Exception: pass
        termios.tcsetattr(fd, termios.TCSADRAIN, old)

if __name__ == "__main__":
    try: main()
    except KeyboardInterrupt: pass
