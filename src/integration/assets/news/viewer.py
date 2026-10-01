#!/usr/bin/env python3
"""herdr AI news viewer: the page set like a leaf from an old almanac, with a timeline spine.

One idea: marginalia. At 100+ columns the source and time of every story are
set as side notes in the outer margin, aligned with the headline; at 160+ the
page opens into a two-page spread with a gutter, notes in the outer margins of
each leaf. Below 100 columns the notes fall inline under each story.

What's new since you read: every story carries a chip in a left-hand spine saying
when it first appeared in the paper (13:03, 11:18 … yest.), so newness reads as a
gradient — this edition in rust on a tinted ground, the one you last read in gold,
older ones fading into the rule colour. Stories that were not in the edition you
last read get a ● node and a rust rail beside the whole story; stories whose text
changed (or that the editor flagged `changed`) get a ◑ node, the editor's "what
changed" note and a one-line word diff (old words struck, new ones gold). The title
page says "● N new ◑ M updated since you read the HH:MM edition" over a first-seen
ribbon with its legend; section rules carry per-section counts; the footer keeps
the tally.

Data beside page.json (the news home): editions/index.json and the edition files;
first_seen.json (the runner's url → first edition/time map; without it the spine
falls back to scanning the editions); read.json (herdr's record of the last edition
you read: {"last_read_edition": N}); viewer-state.json (written here: the edition on
screen, so herdr can record what you read when the News tab is focused).

Everything else is quiet: a title page with fleurons, a rust drop cap on the
lead, justified body text, ornamental section rules, a manicule (☞) pointing at
the selected story, and a folio line at the bottom (theme, edition, leaf).

Editions: left/right step days, up/down step editions, l jumps to the latest;
j/k, the mouse wheel, space/b and PgUp/PgDn scroll.

Usage: python3 viewer.py page.json [--edition N] [--theme T]   interactive
       python3 viewer.py page.json --pinned                     in herdr's News tab (see below)
       python3 viewer.py page.json --dump W                     print the page at width W

Keys: j/k or the mouse wheel scroll, space/b or PgDn/PgUp leaf forward/back, g/G top/bottom,
n/N next/previous new-or-updated story, u new-only on/off (folded count shown),
left/right previous/next day, up/down previous/next edition, l the latest edition,
tab/shift-tab select a story, enter opens it in the browser, t cycles the theme (saved
beside page.json), r reloads, q quits. The page re-reads itself when a new edition lands.

Pinned (--pinned, how herdr runs it in the News tab): q, Esc and Ctrl-C/Ctrl-Z/Ctrl-\\ do
nothing (ISIG is off), so the tab cannot be quit by accident; only herdr's private sequence
CSI 9999 ~ (HERDR_QUIT) ends the viewer, and herdr sends it before typing the next command.
"""
import difflib, json, os, re, select, signal, subprocess, sys, termios, time, tty, unicodedata
from collections import OrderedDict
from datetime import datetime, timedelta

# ------------------------------------------------------------------ arguments
def parse_args(argv):
    path, dump = None, None
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--pinned":
            os.environ["ALMANAC_PINNED"] = "1"
        elif a == "--edition":
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

def col(c): return C[c] if isinstance(c, str) else c

def mix(a, b, t):
    """Blend two colours (palette keys or rgb tuples), t=0 -> a, t=1 -> b."""
    a, b = col(a), col(b)
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))

def sgr(fg=None, bold=False, italic=False, bg=None, strike=False):
    p = ["0"]
    if bold: p.append("1")
    if italic: p.append("3")
    if strike: p.append("9")
    if fg: p.append("38;2;%d;%d;%d" % col(fg))
    if bg: p.append("48;2;%d;%d;%d" % col(bg))
    elif "page" in C: p.append("48;2;%d;%d;%d" % C["page"])
    return "\x1b[" + ";".join(p) + "m"

# ------------------------------------------------------------------ the "new" layer: first-seen times
# MK: story key -> dict(state='new'|'upd'|None, first=datetime|None, rank=editions ago (0 = this one),
#     old=previous text for 'upd', note=the editor's "what changed" line). META: since (datetime of the
#     baseline), ref (date of this edition), at (datetime of this edition), buckets [(label, rank, count)].
MK = {}
META = {"since": None, "ref": None, "at": None, "buckets": [], "new": 0, "upd": 0}
FILTER = [False]                              # u: show only new + updated
STATES = []                                   # per displayed story index, filled by build()

def item_key(it):
    """A story's identity across editions: its url, else its headline without spacing/case noise."""
    return it.get("url") or norm(it.get("head", "")).lower()
def norm(t): return " ".join((t or "").split())
def all_items(page):
    out = [page["lead"]] if page.get("lead") else []
    for s in page.get("sections", []): out += s.get("items", [])
    return out
def item_text(it): return norm(it.get("standfirst", "") + " " + it.get("text", ""))

def compute_marks(page, baseline, history, first_seen=None):
    """page: the edition shown. baseline: the page last read (None: nothing is new).
    history: [(datetime, page)] for editions up to and including this one, oldest first (the
    fallback source of first-seen times). first_seen: {key: (datetime, rank)} from the runner's
    first_seen.json, when it is there."""
    MK.clear()
    at = local(page.get("updated", "")) or (history[-1][0] if history else None)
    META.update(at=at, ref=at.date() if at else None, since=local(baseline.get("updated", "")) if baseline else None)
    first = dict(first_seen or {})
    if not first:
        for k, (t, pg) in enumerate(history):
            for it in all_items(pg): first.setdefault(item_key(it), (t, len(history) - 1 - k))
    prev = {item_key(it): it for it in all_items(baseline)} if baseline else None
    for it in all_items(page):
        key = item_key(it)
        state, old, note = None, None, None
        if prev is not None:
            if key not in prev: state = "new"
            elif item_text(prev[key]) != item_text(it) or it.get("changed") is True:
                state, old = "upd", item_text(prev[key])
                note = norm(it.get("what_changed", "")) if isinstance(it.get("what_changed"), str) else None
        t, rank = first.get(key, (None, None))
        if state == "new" and key not in first: t, rank = at, 0   # no first-seen record: new to the reader = this edition
        MK[key] = dict(state=state, first=t, rank=rank, old=old, note=note)
    META["new"] = sum(1 for m in MK.values() if m["state"] == "new")
    META["upd"] = sum(1 for m in MK.values() if m["state"] == "upd")
    # first-seen buckets: each edition of this day on its own, everything older in one "yest." / "older"
    b = {}
    for m in MK.values():
        lab = chip_label(m["first"])
        r = m["rank"] if m["rank"] is not None else 99
        lab0, r0, n0 = b.get(lab, (lab, r, 0))
        b[lab] = (lab, min(r, r0), n0 + 1)
    META["buckets"] = sorted(b.values(), key=lambda x: x[1])

def chip_label(t):
    if t is None: return "older"
    ref = META["ref"]
    if ref is None or t.date() == ref: return t.strftime("%H:%M")
    if t.date() == ref - timedelta(days=1): return "yest."
    return t.strftime("%a")

def age_colour(m):
    """The gradient: this edition rust, the last-read one gold, then note, dim, rule."""
    r = m.get("rank")
    if m.get("state") == "new" or r == 0: return col("rust")
    if r is None: return col("rule")
    if META["ref"] and m["first"] and m["first"].date() < META["ref"]:
        return mix("dim", "rule", 0.55)
    stops = [col("rust"), col("gold"), mix("gold", "note", 0.6), col("note"), mix("note", "dim", 0.6), col("dim")]
    return stops[min(r, len(stops) - 1)]

CHIP = 6                                      # chip cells; the spine is CHIP + " " + node + " "
SPINE = CHIP + 3

def spine(m, n, body):
    """The spine segments for row n of a story (n=0: headline row). body: row belongs to the story."""
    if m is None: return [seg(" " * (CHIP + 1)), seg("│", sgr("rule")), seg(" ")]
    st = m.get("state")
    c = age_colour(m)
    if n == 0:
        if st == "new":
            lab = chip_label(m["first"])
            chip = [seg(" " * (CHIP - width(lab))), seg(lab, sgr(c, bold=True, bg=mix("page", "rust", 0.20)))]
            node = seg("●", sgr("rust", bold=True))
        elif st == "upd":
            lab = "↻" + (META["at"].strftime("%H:%M") if META["at"] else "")
            chip = [seg(" " * (CHIP - width(lab))), seg(lab, sgr("gold", bg=mix("page", "gold", 0.14)))]
            node = seg("◑", sgr("gold", bold=True))
        else:
            lab = chip_label(m["first"])
            chip = [seg(" " * (CHIP - width(lab))), seg(lab, sgr(c))]
            node = seg("○" if (m.get("rank") or 0) > 1 or lab == "yest." else "◦", sgr(c))
            if m.get("rank") == 1: node = seg("○", sgr("gold"))
        return chip + [seg(" "), node, seg(" ")]
    if body and st == "new": rail = seg("┃", sgr(mix("rust", "page", 0.25)))
    elif body and st == "upd": rail = seg("┃", sgr(mix("gold", "page", 0.35)))
    else: rail = seg("│", sgr("rule"))
    return [seg(" " * (CHIP + 1)), rail, seg(" ")]

def word_diff(old, new, T):
    """One line: a little context, the removed words struck through, the added words in gold."""
    a, b = old.split(), new.split()
    ops = [o for o in difflib.SequenceMatcher(None, a, b).get_opcodes() if o[0] != "equal"]
    if not ops: return []
    i1, j1 = ops[0][1], ops[0][3]
    i2, j2 = ops[-1][2], ops[-1][4]
    if (i2 - i1) + (j2 - j1) > 24:            # a rewrite, not an edit: show the first change only
        i2, j2 = ops[0][2], ops[0][4]
    ctx = 2
    out = []
    if i1 - ctx > 0: out.append(seg("… ", sgr("dim")))
    out.append(seg(" ".join(a[max(0, i1 - ctx):i1]) + " ", sgr("dim", italic=True)))
    for tag, x1, x2, y1, y2 in difflib.SequenceMatcher(None, a[i1:i2], b[j1:j2]).get_opcodes():
        if tag == "equal": out.append(seg(" ".join(a[i1 + x1:i1 + x2]) + " ", sgr("dim", italic=True))); continue
        if x2 > x1: out.append(seg(" ".join(a[i1 + x1:i1 + x2]), sgr("dim", strike=True))); out.append(seg(" "))
        if y2 > y1: out.append(seg(" ".join(b[j1 + y1:j1 + y2]), sgr("gold"))); out.append(seg(" "))
    out.append(seg(" ".join(b[j2:j2 + ctx]), sgr("dim", italic=True)))
    if j2 + ctx < len(b): out.append(seg(" …", sgr("dim")))
    # fit to T cells
    fit, used = [], 0
    for t, st, u in out:
        if used + width(t) > T:
            fit.append((cut_cells(t, max(0, T - used - 1)) + "…", st, u)); break
        fit.append((t, st, u)); used += width(t)
    return fit

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

# A row is a list of segments (text, style, url). Text and URL come from the agent's JSON:
# C0/C1 control characters are dropped so nothing can inject escape sequences or end the OSC 8 link.
CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f]")
def seg(t, st=None, url=None):
    return (CONTROL.sub("", t), sgr() if st is None else st, CONTROL.sub("", url) if url else url)
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

def fmt_story_time(iso, today=None):
    """A story's time with its day: `14:05` today, `yesterday 14:05`, `Sun 14:05` inside the week,
    `27 Sep` when older than six days. Fits a 16-cell margin note."""
    d = local(iso)
    if not d: return ""
    days = ((today or datetime.now().astimezone().date()) - d.date()).days
    if days <= 0: return d.strftime("%H:%M")
    if days == 1: return "yesterday " + d.strftime("%H:%M")
    if days <= 6: return d.strftime("%a %H:%M")
    return "%d %s" % (d.day, d.strftime("%b"))

# 3-row half-block capitals for the drop cap.
FONT = {
    "A": ["▄▀▀▄", "█▀▀█", "▀  ▀"], "B": ["█▀▀▄", "█▀▀▄", "▀▀▀ "], "C": ["▄▀▀▀", "█   ", " ▀▀▀"],
    "D": ["█▀▀▄", "█  █", "▀▀▀ "], "E": ["█▀▀▀", "█▀▀ ", "▀▀▀▀"], "F": ["█▀▀▀", "█▀▀ ", "▀   "],
    "G": ["▄▀▀▀", "█ ▀█", " ▀▀▀"], "H": ["█  █", "█▀▀█", "▀  ▀"], "I": ["▀█▀", " █ ", "▀▀▀"],
    "J": [" ▀█▀", "  █ ", "▀▀  "], "K": ["█ ▄▀", "█▀▄ ", "▀  ▀"], "L": ["█   ", "█   ", "▀▀▀▀"],
    "M": ["█▄ ▄█", "█ ▀ █", "▀   ▀"], "N": ["█▄  █", "█ ▀▄█", "▀   ▀"], "O": ["▄▀▀▄", "█  █", " ▀▀ "],
    "P": ["█▀▀▄", "█▀▀▀", "▀   "], "Q": ["▄▀▀▄", "█  █", " ▀▀▄"], "R": ["█▀▀▄", "█▀▀▄", "▀  ▀"],
    "S": ["▄▀▀▀", " ▀▀▄", "▀▀▀ "], "T": ["▀█▀", " █ ", " ▀ "], "U": ["█  █", "█  █", " ▀▀ "],
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
    gut = 2 + SPINE                           # timeline spine + manicule gutter beside the text
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
    m = MK.get(item_key(item))
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
    src, t = item.get("source", "").strip(), fmt_story_time(item.get("time", ""), META["ref"])
    if m and m.get("state") == "upd":
        # before/after hint: the editor's one-line note, then the word diff against the last-read text
        note = m.get("note") or "Text revised since %s." % (META["since"].strftime("%H:%M") if META["since"] else "the last edition")
        fl = chip_label(m["first"]) if m.get("first") else ""
        lead_s = "↻ revised %s" % (META["at"].strftime("%H:%M") if META["at"] else "")
        tail = ("  ·  first seen " + fl) if fl else ""
        for k, l in enumerate(wrap_plain(lead_s + "  " + note + tail, T)):
            if k == 0 and l.startswith(lead_s):
                rows.append(("  ", [seg(lead_s, sgr("gold", bold=True)), seg(l[len(lead_s):], sgr("note", italic=True))], None))
            else:
                rows.append(("  ", [seg(l, sgr("note", italic=True))], None))
        d = word_diff(m["old"] or "", item_text(item), T - 2)
        if d: rows.append(("  ", [seg("Δ ", sgr("gold"))] + d, None))
    if mode == "inline":
        meta = src + (", " + t if src and t else t)
        if meta: rows.append(("  ", [seg("— " + meta, sgr("dim", italic=True))], None))
    else:
        notes = [[seg(margin_name(src, MW), sgr("note", italic=True))]] if src else []
        if t: notes.append([seg(t, sgr("dim"))])
        while len(rows) < len(notes): rows.append(("  ", [], None))
        if notes:
            r0 = rows[0]
            rows[0] = (r0[0], r0[1], notes)
    rows = [([*spine(m, n, True), seg(g_, sgr("rust"))], tx, nt) for n, (g_, tx, nt) in enumerate(rows)]
    rows.append(("  ", [], None))                        # breathing room after the story
    return block(rows, heads, sec=sec)

def margin_name(src, w):
    """A source name for the margin: one line. Drop a trailing '(via …)'-style note, then cut with …"""
    if width(src) > w:
        src = re.sub(r"\s*\([^)]*\)\s*$", "", src).strip() or src
    if width(src) > w:
        src = cut_cells(src, max(1, w - 1)).rstrip() + "…"
    return src

def section_block(title, n, g, first, counts=None):
    T = g["T"]
    tag = []
    if counts and counts[0]: tag += [seg(" ●%d new" % counts[0], sgr("rust"))]
    if counts and counts[1]: tag += [seg(" ◑%d upd" % counts[1], sgr("gold"))]
    if tag: tag += [seg(" ")]
    label = spaced(title)
    if width(label) + 16 + row_width(tag) > T: label = title.upper()
    mid = [seg("  ✦  ", sgr("gold")), seg(label, sgr("gold")), seg("  ✦  ", sgr("gold"))]
    fill = T - row_width(mid) - row_width(tag) - (1 if tag else 0)
    l = max(0, (T - row_width(mid)) // 2)
    if l > fill - 3: l = max(0, fill // 2)       # narrow leaf: give the tag room, keep both rules
    r = max(0, fill - l)
    rule = [seg("─" * l, sgr("rule"))] + mid + [seg("─" * r, sgr("rule"))] + tag + ([seg("─", sgr("rule"))] if tag else [])
    rows = [] if first else [("  ", [], None)]
    rows += [("  ", rule, None), ("  ", [], None)]
    return block(rows, sec=n, keep=True)

# ------------------------------------------------------------------ title page and colophon
def title_rows(page, tw):
    """Full-width rows for the title page. Returns (rows, ornament_row_index)."""
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
    for r in summary_rows(tw): add(r)
    orn = len(rows)
    add(ornament(tw, 1.0))
    add([])
    since = [" ".join(s.split()) for s in (page.get("since_last_run") or []) if s and s.strip()]
    if since:
        nw = min(tw, 64)
        rows.append(center(pad([seg("To the reader", sgr("gold", italic=True))], nw), tw))
        for entry in since:
            for i, l in enumerate(wrap_plain(entry, nw - 2)):
                lead = "– " if i == 0 else "  "
                rows.append(center(pad([seg(lead, sgr("gold")), seg(l, sgr("body", italic=True))], nw), tw))
        add([])
    return rows, orn

def summary_rows(tw):
    """The compact header: counts since the last read, then a first-seen ribbon and its legend."""
    rows = []
    if META["since"] is None and not META["buckets"]: return rows
    if META["since"] is not None:
        line = [seg("●", sgr("rust", bold=True)), seg(" %d new" % META["new"], sgr("ink", bold=True)),
                seg("   ◑", sgr("gold", bold=True)), seg(" %d updated" % META["upd"], sgr("ink")),
                seg("   since you read the %s edition" % META["since"].strftime("%H:%M"), sgr("note", italic=True))]
        rows.append(line)
    bk = META["buckets"]
    total = sum(n for _, _, n in bk) or 1
    rw = min(tw - 4, 56)
    # ribbon: one run of ▬ per first-seen bucket, newest on the left, widths by share (min 1)
    widths = [max(1, round(rw * n / total)) for _, _, n in bk]
    while sum(widths) > rw: widths[widths.index(max(widths))] -= 1
    ribbon = []
    for (lab, r, n), w in zip(bk, widths):
        c = age_colour(dict(rank=r, first=None, state=None)) if lab not in ("yest.", "older") and not re.match(r"^[A-Z][a-z]{2}$", lab) else mix("dim", "rule", 0.55)
        ribbon.append(seg("▬" * w, sgr(c)))
    rows.append([])
    rows.append([seg("first seen  ", sgr("dim", italic=True))] + ribbon)
    legend, used = [], 0
    for k, (lab, r, n) in enumerate(bk):
        c = age_colour(dict(rank=r, first=None, state=None)) if lab not in ("yest.", "older") and not re.match(r"^[A-Z][a-z]{2}$", lab) else mix("dim", "rule", 0.55)
        piece = [seg(lab, sgr(c, bold=(r == 0))), seg(" %d" % n, sgr("dim"))]
        pw = row_width(piece) + (3 if legend else 0)
        if used + pw > tw - 4 and legend:
            rows.append(legend); legend, used = [], 0; pw = row_width(piece)
        if legend: legend.append(seg(" · ", sgr("rule")))
        legend += piece; used += pw
    if legend: rows.append(legend)
    rows.append([])
    return rows

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
            gs = list(gutter) if isinstance(gutter, list) else spine(None, 1, False) + [seg(gutter, sgr("rust"))]
            nl = note_at.get(k)
            if side == "inline":
                out.append(gs + text)
            elif side == "right":
                r = gs + pad(text, T)
                if nl: r += [seg("   ")] + nl
                out.append(r)
            else:
                n = rjust(nl, MW) if nl else [seg(" " * MW)]
                out.append(n + [seg("   ")] + gs + pad(text, T))
            secs.append(b["sec"])
        for i, off in b["heads"]:
            targets.append((i, base + off - skipped))
    return out, secs, targets

def deal_sections(blocks):
    """Deal whole sections onto the two leaves like a newspaper: left, right, left, right…, each
    section going to the leaf that is shorter so far (ties go left). Anything before the first
    section (the lead, the new-only notice) opens the left leaf. A section never splits."""
    groups, cur = [], []
    for b in blocks:
        if b["keep"] and b["sec"] is not None and cur:
            groups.append(cur); cur = []
        cur.append(b)
    if cur: groups.append(cur)
    left, right = [], []
    for k, grp in enumerate(groups):
        opens_with_section = grp[0]["keep"] and grp[0]["sec"] is not None
        if k == 0 and not opens_with_section:
            left += grp; continue
        lh = sum(len(b["rows"]) for b in left); rh = sum(len(b["rows"]) for b in right)
        (left if lh <= rh else right).extend(grp)
    return left, right

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
    STATES.clear()
    def st(it): return (MK.get(item_key(it)) or {}).get("state")
    def shown(it): return not FILTER[0] or st(it) is not None
    idx = 0
    if page.get("lead") and shown(page["lead"]):
        blocks.append(story_block(page["lead"], g, sel, idx, lead=True, sec=-1)); urls.append(page["lead"].get("url")); idx += 1
        STATES.append(st(page["lead"]))
    for n, sec in enumerate(page.get("sections", [])):
        its = [it for it in sec.get("items", []) if shown(it)]
        if not its: continue
        counts = (sum(1 for it in its if st(it) == "new"), sum(1 for it in its if st(it) == "upd"))
        blocks.append(section_block(sec.get("title", ""), n, g, first=not blocks, counts=counts))
        for it in its:
            blocks.append(story_block(it, g, sel, idx, sec=n)); urls.append(it.get("url")); idx += 1
            STATES.append(st(it))
    if FILTER[0]:
        hidden = len(all_items(page)) - idx
        blocks.insert(0, block([("  ", [seg("new only  ·  %d older stories folded  ·  u shows all" % hidden, sgr("note", italic=True))], None), ("  ", [], None)], keep=True))

    targets = []
    top = len(rows)
    if g["mode"] == "spread":
        lb, rb = deal_sections(blocks)
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
    tally = ""
    if META["since"] is not None:
        tally = "  ● %d new ◑ %d since %s%s" % (META["new"], META["upd"], META["since"].strftime("%H:%M"), " · NEW ONLY" if FILTER[0] else "")
    quit_key = "" if PINNED else " · q quit"
    keys = ("n/N next new · u new only · j/k · ←/→ day · ↑/↓ edition · tab select · enter open · t theme" + quit_key) if W >= 120 else ("n/N new · u only · j/k · ←→ ↑↓ · tab · t" + ("" if PINNED else " · q"))
    keys = cut_cells("  " + keys, max(0, W - width(folio) - width(tally) - 1))
    foot = ("\x1b[%d;1H" % (H + 1) + sgr() + "\x1b[2K" + sgr("rust", bold=True) + tally + sgr("dim") + keys
            + " " * max(0, W - width(tally) - width(keys) - width(folio)) + folio + sgr())
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
    """Split a raw read into key tokens (CSI sequences kept whole). Returns (tokens, rest): an
    escape sequence cut off by the end of the read is handed back to be prepended to the next one
    (herdr's 7-byte quit sequence can straddle two reads)."""
    out, i = [], 0
    while i < len(buf):
        if buf[i] == "\x1b" and (i + 1 == len(buf) or buf[i + 1] in "[O"):
            j = i + 2
            while j < len(buf) and not ("@" <= buf[j] <= "~"): j += 1
            if j >= len(buf): return out, buf[i:]  # incomplete: wait for the rest
            tok = buf[i:j + 1]; i = j + 1
            if tok.startswith("\x1b[<"):          # SGR mouse report: keep only the wheel
                button = tok[3:].split(";", 1)[0]
                if button in ("64", "65"): out.append("\x1b[<" + button)
                continue
            out.append(tok)
        else:
            out.append(buf[i]); i += 1
    return out, ""

def load(path):
    with open(path) as f: return json.load(f)

# ------------------------------------------------------------------ edition history
HIST = {"dir": None, "eds": [], "cur": -1, "home": None}

def load_history(path):
    """editions/index.json beside page.json -> list of editions, oldest first."""
    d = os.path.join(os.path.dirname(os.path.abspath(path)), "editions")
    idx = os.path.join(d, "index.json")
    if not os.path.exists(idx): return None, []
    try:
        with open(idx) as f: return d, json.load(f).get("editions", [])
    except (OSError, json.JSONDecodeError): return d, []

PAGES = OrderedDict()   # path -> page, most recently used last
PAGES_MAX = 8           # the shown and baseline pages plus a few neighbours; a history scan streams through
def read_page(path):
    """An edition file, cached by path (edition files never change once written); the cache is bounded."""
    if path in PAGES:
        PAGES.move_to_end(path)
        return PAGES[path]
    with open(path) as f: pg = json.load(f)
    PAGES[path] = pg
    while len(PAGES) > PAGES_MAX: PAGES.popitem(last=False)
    return pg

def edition_page(i):
    e = HIST["eds"][i]
    return read_page(os.path.join(HIST["dir"], e["path"]))

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
    try: at = datetime.fromisoformat(e["at"]).astimezone().strftime("%a %-d %b %H:%M")
    except Exception: at = e.get("at", "")
    tag = "" if i == len(eds) - 1 else "  (l: latest)"
    return "%s  ·  %s of %s%s" % (at, roman(e["edition"]), roman(eds[-1]["edition"]), tag)

def ed_time(e):
    try: return datetime.fromisoformat(e["at"]).astimezone()
    except Exception: return None

def read_json_soft(path, default):
    try:
        with open(path) as f: return json.load(f)
    except (OSError, json.JSONDecodeError, ValueError): return default

def last_read_edition():
    """herdr's record of the last edition the reader actually looked at (read.json beside page.json)."""
    if not HIST["home"]: return None
    v = read_json_soft(os.path.join(HIST["home"], "read.json"), {})
    v = v.get("last_read_edition") if isinstance(v, dict) else None
    return v if isinstance(v, int) and v > 0 else None

def first_seen_map(upto):
    """{story key: (datetime, rank)} from the runner's first_seen.json, ranked against the editions
    up to and including index `upto` (0 = that edition, 1 = the one before, …). None when the
    file is missing or empty, so the caller falls back to scanning the editions."""
    if not HIST["home"]: return None
    raw = read_json_soft(os.path.join(HIST["home"], "first_seen.json"), {})
    entries = raw.get("stories") if isinstance(raw, dict) else None
    if not isinstance(entries, dict) or not entries: return None
    order = [e.get("edition") for e in HIST["eds"][:upto + 1]]
    rank_of = {n: len(order) - 1 - k for k, n in enumerate(order)}
    known = [x for x in order if isinstance(x, int)]
    out = {}
    for key, v in entries.items():
        if not isinstance(v, dict): continue
        n = v.get("edition")
        t = local(v.get("at", "")) if isinstance(v.get("at"), str) else None
        if n in rank_of: out[key] = (t, rank_of[n])
        elif isinstance(n, int) and known and n < min(known):
            out[key] = (t, len(order))               # before the first edition we know: "older"
    return out or None

def baseline_index(i):
    """The edition to compare edition i against: the one herdr says you last read, when that is
    older than i and in the history; otherwise the edition just before i (None for the first)."""
    eds = HIST["eds"]
    lr = last_read_edition()
    if lr is not None and eds[i].get("edition", 0) > lr:
        j = next((k for k, e in enumerate(eds) if e.get("edition") == lr), None)
        if j is not None and j < i: return j
    return i - 1 if i > 0 else None

def open_edition(i):
    """Load edition i of the history, compute its marks and remember it as the one on screen."""
    eds = HIST["eds"]
    page = edition_page(i)
    j = baseline_index(i)
    base = edition_page(j) if j is not None else None
    first = first_seen_map(i)
    if first:
        history = [(ed_time(eds[i]), page)]          # the runner's log ranks the stories: no scan
    else:
        history = []                                 # no first_seen.json: scan the editions up to this one
        for k in range(i + 1):
            try: history.append((ed_time(eds[k]), edition_page(k)))
            except (OSError, json.JSONDecodeError): pass
    compute_marks(page, base, history, first)
    HIST["cur"] = i
    note_showing(eds[i].get("edition"))
    return page

def note_showing(edition):
    """viewer-state.json beside page.json: what is on screen, for herdr's "last read" record.
    Fail-soft (a read-only home just loses the record)."""
    if not HIST["home"] or not isinstance(edition, int): return
    path = os.path.join(HIST["home"], "viewer-state.json")
    try:
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump({"version": 1, "showing": edition, "at": datetime.now().astimezone().isoformat(timespec="seconds")}, f)
        os.replace(tmp, path)
    except OSError:
        pass

def jump_new(sel, targets, d):
    """Next/previous story (in reading order) that is new or updated; wraps."""
    marked = [i for i, _, _ in targets if i < len(STATES) and STATES[i]]
    if not marked: return sel
    if sel < 0: return marked[0] if d > 0 else marked[-1]
    later = [i for i in marked if (i > sel if d > 0 else i < sel)]
    if later: return later[0] if d > 0 else later[-1]
    return marked[0] if d > 0 else marked[-1]

# herdr ends a pinned viewer with this sequence (CSI 9999 ~), which no key produces.
HERDR_QUIT = "\x1b[9999~"
UP, DOWN = ("\x1b[A", "\x1bOA"), ("\x1b[B", "\x1bOB")
RIGHT, LEFT = ("\x1b[C", "\x1bOC"), ("\x1b[D", "\x1bOD")
PINNED = False

def main():
    global PINNED
    path, dump_w = parse_args(sys.argv[1:])
    PINNED = os.environ.get("ALMANAC_PINNED") == "1"
    home = os.path.dirname(os.path.abspath(path))
    HIST["home"] = home
    THEME_FILE[0] = os.path.join(home, "viewer-theme")
    set_theme(os.environ.get("ALMANAC_THEME") or saved_theme() or THEME_ORDER[0])
    try: sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception: pass
    HIST["dir"], HIST["eds"] = load_history(path)
    if HIST["eds"]:
        want = os.environ.get("ALMANAC_EDITION")
        start = next((i for i, e in enumerate(HIST["eds"]) if str(e["edition"]) == want), len(HIST["eds"]) - 1)
        page = open_edition(start)
    else:
        page = load(path)
        compute_marks(page, None, [])
    if dump_w is not None:
        dump(page, max(20, dump_w)); return

    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        # Everything from here on runs under the finally: whatever fails, the pane's shell gets
        # its terminal back (cooked mode, ISIG, mouse off, main screen).
        rp, wp = os.pipe(); os.set_blocking(wp, False)
        signal.set_wakeup_fd(wp)
        signal.signal(signal.SIGWINCH, lambda *_: None)
        tty.setcbreak(fd)
        if PINNED:
            # Pinned in herdr's News tab: Ctrl-C, Ctrl-Z and Ctrl-\ arrive as plain bytes and do
            # nothing; only herdr's private quit sequence ends the viewer.
            attrs = termios.tcgetattr(fd)
            attrs[3] &= ~(termios.ISIG | termios.IEXTEN)
            termios.tcsetattr(fd, termios.TCSANOW, attrs)
        sys.stdout.write("\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h"); sys.stdout.flush()
        watch = os.path.join(HIST["dir"], "index.json") if HIST["eds"] else path
        mtime = os.path.getmtime(watch)
        off, sel, intro = 0, -1, True
        carry = ""                                 # an escape sequence cut off by the last read
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
                        # a new edition: the baseline (what you had read) is re-read with it
                        page = open_edition(len(HIST["eds"]) - 1 if at_latest else min(HIST["cur"], len(HIST["eds"]) - 1))
                    else:
                        page = load(path)
                    sel = min(sel, len(targets) - 1)
            except (OSError, json.JSONDecodeError): pass
            if fd not in ready: continue
            buf = carry + os.read(fd, 64).decode(errors="ignore")
            keys, carry = tokens(buf)
            quit_ = False
            for k in keys:
                if k == HERDR_QUIT or (k == "q" and not PINNED): quit_ = True; break
                elif k in ("j", "\x1b[<65"): off += 1 if k == "j" else 3
                elif k in ("k", "\x1b[<64"): off -= 1 if k == "k" else 3
                elif k in (" ", "\x1b[6~"): off += max(1, H - 2)
                elif k in ("b", "\x1b[5~"): off -= max(1, H - 2)
                elif k == "t":
                    i = THEME_ORDER.index(THEME[0])
                    set_theme(THEME_ORDER[(i + 1) % len(THEME_ORDER)]); save_theme(THEME[0])
                elif k == "u":
                    FILTER[0] = not FILTER[0]; sel, off = -1, 0
                    rows, secs, targets, orn = build(page, W, sel); maxoff = max(0, len(rows) - H)
                elif k in ("n", "N") and targets:
                    sel = jump_new(sel, targets, 1 if k == "n" else -1)
                    row = next((y for i, y, _ in targets if i == sel), None)
                    if row is not None: off = max(0, row - H // 3)
                elif (k in LEFT + RIGHT + UP + DOWN or k == "l") and HIST["eds"]:
                    # left/right: previous/next day; up/down: previous/next edition; l: latest
                    i = HIST["cur"]
                    if k in LEFT: j = step_day(i, -1)
                    elif k in RIGHT: j = step_day(i, 1)
                    elif k in UP: j = max(0, i - 1)
                    elif k in DOWN: j = min(len(HIST["eds"]) - 1, i + 1)
                    else: j = len(HIST["eds"]) - 1
                    if j != i:
                        try: page = open_edition(j); off, sel = 0, -1
                        except (OSError, json.JSONDecodeError): pass
                elif k == "g": off = 0
                elif k == "G": off = maxoff
                elif k == "r":
                    try:
                        PAGES.clear()
                        if HIST["eds"]:
                            HIST["dir"], eds = load_history(path)
                            if eds: HIST["eds"] = eds
                            page = open_edition(min(HIST["cur"], len(HIST["eds"]) - 1))
                        else:
                            page = load(path)
                        mtime = os.path.getmtime(watch)
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
        sys.stdout.write("\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l"); sys.stdout.flush()
        try: signal.set_wakeup_fd(-1)
        except Exception: pass
        termios.tcsetattr(fd, termios.TCSADRAIN, old)

if __name__ == "__main__":
    try: main()
    except KeyboardInterrupt: pass
