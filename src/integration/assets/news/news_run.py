#!/usr/bin/env python3
"""herdr AI news: one run of the news desk.

Prepares a run directory, fetches anchors, runs the editor agent (`claude -p`) with its progress
streamed to this terminal, validates what it wrote, publishes a new edition, and then shows the page.

usage: news_run.py --home DIR [--trigger manual|scheduled] [--model M] [--deadline-min N] [--dry-run] [--no-view]

Home layout (DIR):
  topic.md, sources.json            editorial profile and source list (seeded from assets, owner-edited)
  page.json                         latest edition (what the viewer opens)
  notes.md, seen.json               the agent's memory
  editions/index.json               every published edition (never pruned)
  editions/<YYYY-MM-DD>/<HHMM>-eNNNN.json
  runs/<stamp>/                     per-run working directory, inputs, out/, stream.jsonl, result.json
  runs/index.jsonl                  one line per run: outcome, cost, turns, edition
  http-cache.json                   conditional-request cache for anchors
"""
import argparse, fcntl, json, os, re, select, shutil, signal, subprocess, sys, time
from datetime import datetime, timedelta, timezone

ASSETS = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True  # never leave __pycache__ in the asset directory
sys.path.insert(0, ASSETS)
import anchors  # noqa: E402

LOCAL = datetime.now().astimezone().tzinfo
SECTIONS = ["Top", "Models and labs", "Agent tooling", "Papers", "Infra and policy", "Watching"]
DEADLINE_MIN = 60           # one budget for the whole run (both editor calls share it); herdr passes --deadline-min
CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f]")
URL_RE = re.compile(r"^https://[^\s\x00-\x1f\x7f]+$")
TOOLS = "WebSearch WebFetch ToolSearch Read Write Edit Glob Grep Agent"
BLOCKED = "Bash PowerShell NotebookEdit"

# ------------------------------------------------------------------ terminal output
def c(code, s): return "\x1b[%sm%s\x1b[0m" % (code, s) if sys.stdout.isatty() else s
def say(msg, tone="0"):
    stamp = datetime.now().strftime("%H:%M:%S")
    print("%s  %s" % (c("2", stamp), c(tone, msg)), flush=True)

# ------------------------------------------------------------------ herdr reporting
class Herdr:
    """Reports this run to the herdr server that owns the pane, so herdr shows it as a working agent.

    Uses the pane's HERDR_SOCKET_PATH / HERDR_PANE_ID. A no-op outside herdr. Never raises.
    Reports as its own hook source and agent (herdr:news, agent "news"): herdr applies hook state for
    an agent label no screen manifest owns without needing to see a process, whereas a "claude" label
    stays with screen detection (which never sees a `claude -p` child) and the Claude integration's
    own source only carries session identity. No session id is reported, so herdr never persists (or
    resumes on restart) the editor session; the editor itself runs without the pane identity (see
    run_agent), so this runner is the pane's only reporter and no other owner can make herdr drop it.
    """
    SOURCE = "herdr:news"
    AGENT = "news"

    def __init__(self):
        self.sock = os.environ.get("HERDR_SOCKET_PATH")
        self.pane = os.environ.get("HERDR_PANE_ID")
        self.seq = 0
        self.last = 0.0

    def call(self, method, params):
        if not (self.sock and self.pane): return
        import socket
        self.seq = max(self.seq + 1, int(time.time() * 1000))  # per-source sequence: strictly increasing
        req = {"id": "news:%d" % self.seq, "method": method,
               "params": dict(params, pane_id=self.pane, source=self.SOURCE, seq=self.seq)}
        try:
            cl = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); cl.settimeout(0.5)
            cl.connect(self.sock); cl.sendall((json.dumps(req) + "\n").encode())
            try: cl.recv(4096)
            except OSError: pass
            cl.close()
        except OSError: pass

    def state(self, state, message=None, throttle=0.0):
        if throttle and time.time() - self.last < throttle: return
        self.last = time.time()
        p = {"agent": self.AGENT, "state": state}
        if message: p["message"] = message[:120]
        self.call("pane.report_agent", p)

    def title(self, text):
        self.call("pane.report_metadata", {"agent": self.AGENT, "title": text[:60]})

    def release(self):
        self.call("pane.release_agent", {"agent": self.AGENT})

HERDR = Herdr()

# ------------------------------------------------------------------ small helpers
def now_utc(): return datetime.now(timezone.utc)

def read_json(path, default):
    try:
        with open(path) as f: return json.load(f)
    except (OSError, ValueError): return default

def write_json(path, obj):
    tmp = path + ".tmp"
    with open(tmp, "w") as f: json.dump(obj, f, ensure_ascii=False, indent=1)
    os.replace(tmp, path)

def words(s): return len(re.findall(r"\S+", s or ""))

def iso(d): return d.isoformat(timespec="seconds")

def seed(home):
    """First run: copy the editable defaults out of the assets."""
    for name in ("topic.md", "sources.json"):
        dst = os.path.join(home, name)
        if not os.path.exists(dst): shutil.copy(os.path.join(ASSETS, name), dst)
    os.makedirs(os.path.join(home, "editions"), exist_ok=True)
    os.makedirs(os.path.join(home, "runs"), exist_ok=True)

# ------------------------------------------------------------------ history
def editions_index(home):
    return read_json(os.path.join(home, "editions", "index.json"), {"version": 1, "editions": []})

def story_key(it):
    """A story's identity across editions: its url, else its headline without spacing/case noise."""
    return it.get("url") or " ".join((it.get("head") or "").split()).lower()

def first_seen_index(home, idx, edition, at, items):
    """first_seen.json: {"version": 1, "stories": {key: {"edition": n, "at": iso}}} — the edition a
    story first appeared in (never moved later). Missing or unreadable: backfilled once from every
    edition in the index (oldest first), then the new edition's stories are added. The viewer
    reads it for the timeline spine; nothing here needs a model call."""
    doc = read_json(os.path.join(home, "first_seen.json"), None)
    stories = doc.get("stories") if isinstance(doc, dict) and isinstance(doc.get("stories"), dict) else None
    if stories is None:
        stories = {}
        for e in idx.get("editions", []):
            if e.get("edition") == edition: continue
            ed = read_json(os.path.join(home, "editions", e.get("path", "")), None)
            if not ed: continue
            for it in [ed.get("lead")] + [i for s in ed.get("sections", []) for i in s.get("items", [])]:
                if isinstance(it, dict) and story_key(it):
                    stories.setdefault(story_key(it), {"edition": e.get("edition"), "at": e.get("at")})
    for it in items:
        k = story_key(it)
        if k: stories.setdefault(k, {"edition": edition, "at": at})
    return {"version": 1, "stories": stories}

def history_digest(home, days=7):
    idx = editions_index(home)["editions"]
    cutoff = now_utc() - timedelta(days=days)
    out = []
    for e in idx:
        if datetime.fromisoformat(e["at"]) < cutoff: continue
        ed = read_json(os.path.join(home, "editions", e["path"]), None)
        if not ed: continue
        items = [{"section": s["title"], "head": i["head"], "url": i["url"]} for s in ed.get("sections", []) for i in s["items"]]
        out.append({"edition": e["edition"], "at": e["at"],
                    "lead": (ed.get("lead") or {}).get("head"), "items": items})
    return {"days": days, "editions": out}

# ------------------------------------------------------------------ validation
def feed_notes(anchors_doc):
    return {i["url"]: i.get("note", "") for i in anchors_doc.get("items", [])}

def shares_run(a, b, n=8):
    """True if a and b share a run of n consecutive words (case-insensitive)."""
    wa = re.findall(r"\w+", (a or "").lower()); wb = " ".join(re.findall(r"\w+", (b or "").lower()))
    return any(" ".join(wa[i:i + n]) in wb for i in range(0, max(0, len(wa) - n + 1))) if len(wa) >= n else False

def check_item(where, it, notes, errors, lead=False):
    if not isinstance(it, dict):
        errors.append("%s: must be an object" % where); return
    for k in ("head", "text", "url", "source", "time") + (("standfirst",) if lead else ()):
        v = it.get(k)
        if not isinstance(v, str) or not v:
            errors.append("%s: missing %s" % (where, k) if not v else "%s: %s must be a string" % (where, k))
        elif CONTROL.search(v): errors.append("%s: %s contains control characters" % (where, k))
    if not lead and isinstance(it.get("standfirst"), str) and CONTROL.search(it["standfirst"]):
        errors.append("%s: standfirst contains control characters" % where)
    # optional: a carried-over story whose substance changed, with a one-line note (the viewer's ◑)
    if "changed" in it and not isinstance(it["changed"], bool):
        errors.append("%s: changed must be true or false" % where)
    if "what_changed" in it:
        wc = it["what_changed"]
        if not isinstance(wc, str): errors.append("%s: what_changed must be a string" % where)
        elif CONTROL.search(wc): errors.append("%s: what_changed contains control characters" % where)
        elif words(wc) > 30: errors.append("%s: what_changed is %d words (want one line, up to 30)" % (where, words(wc)))
    head, t, url, sf = (it.get(k) if isinstance(it.get(k), str) else "" for k in ("head", "text", "url", "standfirst"))
    if head and words(head) > 16: errors.append("%s: head over 12 words" % where)
    if t.rstrip().endswith(("…", "...")): errors.append("%s: text ends with an ellipsis" % where)
    if t and not 12 <= words(t) <= 60: errors.append("%s: text is %d words (want 25-40)" % (where, words(t)))
    if not URL_RE.match(url): errors.append("%s: url is not a plain https URL" % where)
    if "news.google.com" in url: errors.append("%s: url is a Google News redirect" % where)
    if shares_run(t, notes.get(url, "")): errors.append("%s: text copies the feed description" % where)
    try: datetime.fromisoformat(str(it.get("time", "")).replace("Z", "+00:00"))
    except ValueError: errors.append("%s: time is not ISO 8601" % where)
    if lead and not 30 <= words(sf) <= 90: errors.append("lead: standfirst is %d words (want 45-70)" % words(sf))

def validate(out_dir, anchors_doc):
    errors = []
    page = read_json(os.path.join(out_dir, "page.json"), None)
    decision = read_json(os.path.join(out_dir, "decision.json"), None)
    notes_md = os.path.join(out_dir, "notes.md")
    if page is None: errors.append("out/page.json is missing or not valid JSON")
    elif not isinstance(page, dict): errors.append("out/page.json must be an object"); page = None
    if decision is None: errors.append("out/decision.json is missing or not valid JSON")
    elif not isinstance(decision, dict): errors.append("out/decision.json must be an object"); decision = None
    if not os.path.exists(notes_md) or os.path.getsize(notes_md) == 0: errors.append("out/notes.md is missing or empty")
    elif os.path.getsize(notes_md) > 16000: errors.append("out/notes.md is over 16 KB")
    if page is not None:
        notes = feed_notes(anchors_doc)
        if os.path.getsize(os.path.join(out_dir, "page.json")) > 60000: errors.append("page.json is over 60 KB")
        if not isinstance(page.get("lead"), dict): errors.append("lead is missing")
        else: check_item("lead", page["lead"], notes, errors, lead=True)
        sections = page.get("sections", [])
        if not isinstance(sections, list): errors.append("sections must be a list"); sections = []
        bad = [s for s in sections if not isinstance(s, dict)]
        if bad: errors.append("every section must be an object {title, items}")
        sections = [s for s in sections if isinstance(s, dict)]
        for s in sections:
            if not isinstance(s.get("items", []), list): errors.append("%s: items must be a list" % s.get("title")); s["items"] = []
        titles = [s.get("title") for s in sections]
        if [t for t in titles if t not in SECTIONS]: errors.append("unknown section titles: %s" % [t for t in titles if t not in SECTIONS])
        if [t for t in SECTIONS if t in titles] != titles: errors.append("sections are out of order; want %s" % SECTIONS)
        total = 0
        for s in sections:
            items = s.get("items", [])
            total += len(items)
            if len(items) > 10: errors.append("%s: more than 10 items" % s.get("title"))
            for n, it in enumerate(items, 1): check_item("%s #%d" % (s.get("title"), n), it, notes, errors)
        if total > 40: errors.append("more than 40 items in total")
        allitems = ([page["lead"]] if isinstance(page.get("lead"), dict) else []) + [i for s in sections for i in s.get("items", []) if isinstance(i, dict)]
        unread = sum(1 for i in allitems if not i.get("read"))
        if allitems and unread * 2 > len(allitems):
            errors.append("%d of %d items are read:false; open the sources with WebFetch and summarise from the article" % (unread, len(allitems)))
        slr = page.get("since_last_run", [])
        if not isinstance(slr, list) or len(slr) > 6 or not all(isinstance(x, str) for x in slr):
            errors.append("since_last_run must be a list of at most 6 lines")
        elif any(CONTROL.search(x) for x in slr): errors.append("since_last_run contains control characters")
    if decision is not None:
        if not isinstance(decision.get("changed"), bool): errors.append("decision.changed must be true or false")
        n = decision.get("notify")
        if n is not None and not (isinstance(n, dict) and isinstance(n.get("title"), str) and n.get("title")
                                  and isinstance(n.get("body", ""), str) and n.get("urgency") in ("low", "high")):
            errors.append("decision.notify must be null or {title, body, urgency: low|high}")
        elif isinstance(n, dict) and CONTROL.search(n.get("title", "") + n.get("body", "")):
            errors.append("decision.notify contains control characters")
    src = os.path.join(out_dir, "sources.json")
    if os.path.exists(src):
        s = read_json(src, None)
        if not isinstance(s, list) or not all(isinstance(x, dict) and x.get("id") and x.get("url") and x.get("type") for x in s):
            errors.append("out/sources.json must be a list of {id, url, type, ...}")
        elif len(s) > 40: errors.append("out/sources.json has more than 40 sources")
        elif not all(isinstance(x["url"], str) and URL_RE.match(x["url"]) for x in s):
            errors.append("out/sources.json: every url must be a plain https URL")
    return errors

# ------------------------------------------------------------------ the agent
def describe(tool, inp):
    if tool == "WebFetch": return "read   " + inp.get("url", "")
    if tool == "WebSearch": return "search " + inp.get("query", "")
    if tool in ("Read", "Write", "Edit"): return tool.lower().ljust(6) + " " + os.path.basename(inp.get("file_path", ""))
    if tool == "ToolSearch": return None
    return tool + " " + json.dumps(inp)[:80]

def kill_group(pid, sig):
    """Signal the editor's process group; nothing if it is already gone."""
    try: os.killpg(pid, sig)
    except (ProcessLookupError, PermissionError): pass

def run_agent(run_dir, prompt, model, deadline, resume=None):
    """Run claude -p until `deadline` (a time.time() value shared by the whole run), stream progress,
    return (result_event or None, session_id, timed_out)."""
    with open(os.path.join(ASSETS, "system.md")) as f: system = f.read()
    # Not --restricted: it also strips WebFetch. Shell tools are blocked explicitly instead.
    cmd = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
           "--allowedTools", TOOLS, "--disallowedTools", BLOCKED, "--append-system-prompt", system]
    if model: cmd += ["--model", model]
    if resume: cmd += ["--resume", resume]
    log = open(os.path.join(run_dir, "stream.jsonl"), "ab")
    # The editor runs without herdr's pane identity: the shared Claude hook would otherwise claim the
    # pane's agent session as herdr:claude and herdr would drop this runner's own herdr:news reports.
    env = {k: v for k, v in os.environ.items() if k not in ("HERDR_PANE_ID", "HERDR_TAB_ID", "HERDR_WORKSPACE_ID")}
    proc = subprocess.Popen(cmd, cwd=run_dir, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            start_new_session=True, env=env)
    fd = proc.stdout.fileno()
    result, session, timed_out, reads = None, resume, False, 0
    # Binary reads split into lines here: nothing waits in a Python text buffer.
    partial = b""

    def handle(raw):
        nonlocal result, session, reads
        log.write(raw + b"\n")
        line = raw.decode("utf-8", errors="replace")
        try: e = json.loads(line)
        except ValueError:
            say(line.rstrip()[:160], "2"); return
        t = e.get("type")
        if t == "system" and e.get("subtype") == "init":
            session = e.get("session_id")
            say("editor started (%s, session %s)" % (e.get("model"), (session or "?")[:8]), "1")
            HERDR.state("working", "news editor started")
        elif t == "assistant":
            for part in e.get("message", {}).get("content", []):
                if part.get("type") == "tool_use":
                    d = describe(part.get("name"), part.get("input", {}))
                    if d:
                        reads += part.get("name") == "WebFetch"
                        HERDR.state("working", d, throttle=2.0)
                        say("  " + d[:150], "36" if part.get("name") in ("WebFetch", "WebSearch") else "33")
                elif part.get("type") == "text" and part.get("text", "").strip():
                    say("  " + part["text"].strip().splitlines()[0][:150], "2")
        elif t == "result":
            result = e

    try:
        while True:
            left = deadline - time.time()
            if left <= 0:
                timed_out = True; kill_group(proc.pid, signal.SIGTERM); break
            r, _, _ = select.select([fd], [], [], min(5, left))
            if not r:
                if proc.poll() is not None:
                    if partial: handle(partial); partial = b""
                    break
                continue
            chunk = os.read(fd, 65536)
            if not chunk:                          # EOF: the editor closed its output
                if partial: handle(partial); partial = b""
                if proc.poll() is not None: break
                time.sleep(0.1); continue
            partial += chunk
            *lines, partial = partial.split(b"\n")
            for raw in lines: handle(raw)
    except KeyboardInterrupt:
        kill_group(proc.pid, signal.SIGTERM)
        raise
    finally:
        try: proc.wait(timeout=10)
        except subprocess.TimeoutExpired: kill_group(proc.pid, signal.SIGKILL)
        log.close()
    return result, session, timed_out

# ------------------------------------------------------------------ publish
def since_lines(page, prev):
    if not prev: return ["First edition."]
    old = {i["url"] for s in prev.get("sections", []) for i in s["items"]} | {(prev.get("lead") or {}).get("url")}
    new = {i["url"] for s in page.get("sections", []) for i in s["items"]} | {page["lead"]["url"]}
    return ["%d new stories, %d dropped." % (len(new - old), len(old - new))]

def publish(home, run_dir, page, decision, trigger, anchors_doc, started, next_run=None):
    idx = editions_index(home)
    n = (idx["editions"][-1]["edition"] if idx["editions"] else 0) + 1
    at = now_utc()
    prev = read_json(os.path.join(home, "page.json"), None)
    items = [page["lead"]] + [i for s in page["sections"] for i in s["items"]]
    page.update({"version": 1, "edition": n, "updated": iso(at), "trigger": trigger, "stub": False,
                 "next_run": next_run,
                 "stats": {"anchor_items": len(anchors_doc.get("items", [])),
                           "sources": len(anchors_doc.get("log", {})),
                           "read": sum(1 for i in items if i.get("read"))}})
    if not page.get("since_last_run"): page["since_last_run"] = since_lines(page, prev)
    local = at.astimezone(LOCAL)
    day = local.strftime("%Y-%m-%d")
    rel = "%s/%s-e%04d.json" % (day, local.strftime("%H%M"), n)
    os.makedirs(os.path.join(home, "editions", day), exist_ok=True)
    write_json(os.path.join(home, "editions", rel), page)                # 1. the edition
    idx["editions"].append({"edition": n, "path": rel, "at": iso(at), "day": day, "trigger": trigger,
                            "stories": len(items), "changed": bool(decision.get("changed"))})
    write_json(os.path.join(home, "editions", "index.json"), idx)        # 2. the index
    write_json(os.path.join(home, "page.json"), page)                    # 3. the latest copy
    shutil.copy(os.path.join(run_dir, "out", "notes.md"), os.path.join(home, "notes.md"))
    seen = read_json(os.path.join(home, "seen.json"), {})
    cutoff = iso(at - timedelta(days=30))
    seen = {u: t for u, t in seen.items() if t >= cutoff}
    for i in items: seen.setdefault(i["url"], iso(at))
    write_json(os.path.join(home, "seen.json"), seen)
    write_json(os.path.join(home, "first_seen.json"), first_seen_index(home, idx, n, iso(at), items))
    new_sources = os.path.join(run_dir, "out", "sources.json")
    if os.path.exists(new_sources): shutil.copy(new_sources, os.path.join(home, "sources.json"))
    return n

# ------------------------------------------------------------------ main
CONFIRM_S = 3.0
_last_interrupt = [0.0]

def confirm_interrupt(signum, frame):
    """SIGINT handler for pinned runs: the second Ctrl-C within CONFIRM_S stops the run."""
    now = time.time()
    if now - _last_interrupt[0] <= CONFIRM_S:
        raise KeyboardInterrupt
    _last_interrupt[0] = now
    sys.stdout.write("\n")
    say("press Ctrl-C again within %d s to stop this run" % CONFIRM_S, "33")

def stop_signal(signum, frame):
    """SIGTERM / SIGHUP (herdr's watchdog, the pane or the server going away): stop like a second
    Ctrl-C, so the editor's process group is killed and the run is recorded "interrupted"."""
    raise KeyboardInterrupt

def view(home, a):
    """Replace this process with the page viewer (interactive runs only)."""
    page = os.path.join(home, "page.json")
    if a.no_view or not sys.stdout.isatty() or not os.path.exists(page): return
    try: os.get_terminal_size()                    # the tty may be gone after a SIGHUP
    except OSError: return
    args = [sys.executable, os.path.join(ASSETS, "viewer.py"), page] + (["--pinned"] if a.pinned else [])
    os.execvp(sys.executable, args)

def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--home", required=True)
    ap.add_argument("--trigger", default="manual", choices=["manual", "scheduled"])
    ap.add_argument("--model")
    ap.add_argument("--next-run", help="ISO time of the next scheduled run, shown on the page")
    ap.add_argument("--deadline-min", type=float, default=DEADLINE_MIN,
                    help="minutes the whole run may take (fetch, editor and its fix-up call together)")
    ap.add_argument("--pinned", action="store_true",
                    help="running in herdr's News tab: ignore Ctrl-C/Ctrl-Z and pin the viewer")
    ap.add_argument("--dry-run", action="store_true", help="prepare inputs and fetch anchors only")
    ap.add_argument("--no-view", action="store_true")
    a = ap.parse_args(argv)
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, stop_signal)
    if a.pinned:
        # Pinned in herdr's News tab: Ctrl-Z and Ctrl-\ do nothing, and one stray Ctrl-C
        # does not end the run; a second one within 3 s stops it (recorded "interrupted").
        for sig in (signal.SIGQUIT, signal.SIGTSTP):
            signal.signal(sig, signal.SIG_IGN)
        signal.signal(signal.SIGINT, confirm_interrupt)
    home = os.path.abspath(os.path.expanduser(a.home))
    os.makedirs(home, exist_ok=True)
    seed(home)

    lock = open(os.path.join(home, "run.lock"), "w")
    try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        say("another news run is in progress; not starting a second one", "31"); return 3
    lock.write(str(os.getpid())); lock.flush()

    started = now_utc()
    stamp = started.astimezone(LOCAL).strftime("%Y%m%d-%H%M%S")
    run_dir = os.path.join(home, "runs", stamp)
    os.makedirs(os.path.join(run_dir, "out"))
    record = {"started": iso(started), "trigger": a.trigger, "run": stamp, "outcome": None,
              "cost_usd": 0.0, "turns": 0, "edition": None, "errors": []}

    def finish(outcome, code):
        record.update(outcome=outcome, ended=iso(now_utc()),
                      seconds=int((now_utc() - started).total_seconds()))
        write_json(os.path.join(run_dir, "result.json"), record)
        with open(os.path.join(home, "runs", "index.jsonl"), "a") as f: f.write(json.dumps(record) + "\n")
        tone = "32" if outcome == "ok" else "31"
        HERDR.title("News: edition %d" % record["edition"] if record["edition"] else "News: %s" % outcome)
        HERDR.state("idle", "news run %s" % outcome); HERDR.release()
        say("run %s: %s  (%ds, $%.2f, %d turns%s)" % (stamp, outcome, record["seconds"], record["cost_usd"],
            record["turns"], (", edition %d" % record["edition"]) if record["edition"] else ""), tone)
        return code

    try:
        return desk(a, home, run_dir, stamp, started, record, finish)
    except KeyboardInterrupt:
        print()
        record["errors"] = ["interrupted by the user"]  # USER_INTERRUPT_ERROR in src/app/news.rs: not counted as a failure
        finish("interrupted", 130)
        view(home, a)
        return 130
    except Exception as e:  # a bug or a malformed input: still leave a record
        record["errors"] = ["%s: %s" % (type(e).__name__, e)]
        return finish("crashed", 1)


def desk(a, home, run_dir, stamp, started, record, finish):
    """Fetch, run the editor, validate, publish. Returns the exit code."""
    say("AI news run %s (%s)" % (stamp, a.trigger), "1")
    HERDR.title("News: run %s" % stamp)
    # inputs
    idx = editions_index(home)["editions"]
    last = datetime.fromisoformat(idx[-1]["at"]) if idx else None
    hours = 48 if last is None else min(48, max(12, (started - last).total_seconds() / 3600 + 2))
    say("fetching sources (last %.0f h)" % hours)
    anchors.main(["--sources", os.path.join(home, "sources.json"), "--out", os.path.join(run_dir, "anchors.json"),
                  "--hours", str(hours), "--cache", os.path.join(home, "http-cache.json")])
    anchors_doc = read_json(os.path.join(run_dir, "anchors.json"), {"items": []})
    for name in ("topic.md", "sources.json", "notes.md", "seen.json"):
        src = os.path.join(home, name)
        if os.path.exists(src): shutil.copy(src, os.path.join(run_dir, name))
    if not os.path.exists(os.path.join(run_dir, "notes.md")):
        open(os.path.join(run_dir, "notes.md"), "w").write("# Notes\n\n- First run. No notes yet.\n")
    if os.path.exists(os.path.join(home, "page.json")):
        shutil.copy(os.path.join(home, "page.json"), os.path.join(run_dir, "previous-page.json"))
    write_json(os.path.join(run_dir, "history.json"), history_digest(home))
    if a.dry_run:
        say("dry run: inputs ready in %s" % run_dir)
        return finish("dry-run", 0)

    edition = (idx[-1]["edition"] if idx else 0) + 1
    prompt = ("Run the news desk for edition %d. It is %s. The inputs are in the current directory. "
              "Follow your instructions and topic.md, and write out/page.json, out/notes.md and "
              "out/decision.json (last)." % (edition, started.astimezone(LOCAL).strftime("%A %d %B %Y, %H:%M %Z")))
    total_cost, turns = 0.0, 0
    deadline = started.timestamp() + a.deadline_min * 60   # one budget for the whole run
    result, session, timed_out = run_agent(run_dir, prompt, a.model, deadline)
    for attempt in range(2):
        if result:
            total_cost += result.get("total_cost_usd") or 0; turns += result.get("num_turns") or 0
        record.update(cost_usd=round(total_cost, 4), turns=turns)
        if timed_out:
            record["errors"] = ["deadline: no result after %g minutes" % a.deadline_min]
            return finish("timeout", 4)
        errors = validate(os.path.join(run_dir, "out"), anchors_doc)
        if not errors: break
        record["errors"] = errors
        say("validation found %d problem(s):" % len(errors), "31")
        for e in errors[:12]: say("  - " + e, "31")
        if attempt == 1 or not session:
            return finish("invalid", 5)
        say("asking the editor to fix them", "33")
        fix = ("The runner rejected your output. Fix every problem below by editing the files in out/, "
               "then rewrite out/decision.json last.\n- " + "\n- ".join(errors))
        result, session, timed_out = run_agent(run_dir, fix, a.model, deadline, resume=session)

    page = read_json(os.path.join(run_dir, "out", "page.json"), None)
    decision = read_json(os.path.join(run_dir, "out", "decision.json"), {})
    record["decision"] = decision
    record["edition"] = publish(home, run_dir, page, decision, a.trigger, anchors_doc, started, a.next_run)
    code = finish("ok", 0)
    view(home, a)
    return code


if __name__ == "__main__":
    sys.exit(main())
