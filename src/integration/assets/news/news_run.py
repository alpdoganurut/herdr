#!/usr/bin/env python3
"""herdr AI news: one run of the news desk.

Prepares a run directory, fetches anchors, runs the editor agent (`claude -p`) with its progress
streamed to this terminal, validates what it wrote, publishes a new edition, and then shows the page.

usage: news_run.py --home DIR [--trigger manual|scheduled] [--model M] [--dry-run] [--no-view]

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
WATCHDOG_S = 60 * 60
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
    for k in ("head", "text", "url", "source", "time"):
        if not it.get(k): errors.append("%s: missing %s" % (where, k))
    if it.get("head") and words(it["head"]) > 16: errors.append("%s: head over 12 words" % where)
    t = it.get("text", "")
    if t.rstrip().endswith(("…", "...")): errors.append("%s: text ends with an ellipsis" % where)
    if t and not 12 <= words(t) <= 60: errors.append("%s: text is %d words (want 25-40)" % (where, words(t)))
    if not str(it.get("url", "")).startswith("https://"): errors.append("%s: url is not https" % where)
    if "news.google.com" in str(it.get("url", "")): errors.append("%s: url is a Google News redirect" % where)
    if shares_run(t, notes.get(it.get("url"), "")): errors.append("%s: text copies the feed description" % where)
    try: datetime.fromisoformat(str(it.get("time", "")).replace("Z", "+00:00"))
    except ValueError: errors.append("%s: time is not ISO 8601" % where)
    if lead:
        sf = it.get("standfirst", "")
        if not 30 <= words(sf) <= 90: errors.append("lead: standfirst is %d words (want 45-70)" % words(sf))

def validate(out_dir, anchors_doc):
    errors = []
    page = read_json(os.path.join(out_dir, "page.json"), None)
    decision = read_json(os.path.join(out_dir, "decision.json"), None)
    notes_md = os.path.join(out_dir, "notes.md")
    if page is None: errors.append("out/page.json is missing or not valid JSON")
    if decision is None: errors.append("out/decision.json is missing or not valid JSON")
    if not os.path.exists(notes_md) or os.path.getsize(notes_md) == 0: errors.append("out/notes.md is missing or empty")
    elif os.path.getsize(notes_md) > 16000: errors.append("out/notes.md is over 16 KB")
    if page is not None:
        notes = feed_notes(anchors_doc)
        if os.path.getsize(os.path.join(out_dir, "page.json")) > 60000: errors.append("page.json is over 60 KB")
        if not isinstance(page.get("lead"), dict): errors.append("lead is missing")
        else: check_item("lead", page["lead"], notes, errors, lead=True)
        titles = [s.get("title") for s in page.get("sections", [])]
        if [t for t in titles if t not in SECTIONS]: errors.append("unknown section titles: %s" % [t for t in titles if t not in SECTIONS])
        if [t for t in SECTIONS if t in titles] != titles: errors.append("sections are out of order; want %s" % SECTIONS)
        total = 0
        for s in page.get("sections", []):
            items = s.get("items", [])
            total += len(items)
            if len(items) > 10: errors.append("%s: more than 10 items" % s.get("title"))
            for n, it in enumerate(items, 1): check_item("%s #%d" % (s.get("title"), n), it, notes, errors)
        if total > 40: errors.append("more than 40 items in total")
        allitems = ([page["lead"]] if isinstance(page.get("lead"), dict) else []) + [i for s in page.get("sections", []) for i in s.get("items", [])]
        unread = sum(1 for i in allitems if not i.get("read"))
        if allitems and unread * 2 > len(allitems):
            errors.append("%d of %d items are read:false; open the sources with WebFetch and summarise from the article" % (unread, len(allitems)))
        slr = page.get("since_last_run", [])
        if not isinstance(slr, list) or len(slr) > 6: errors.append("since_last_run must be a list of at most 6 lines")
    if decision is not None:
        if not isinstance(decision.get("changed"), bool): errors.append("decision.changed must be true or false")
        n = decision.get("notify")
        if n is not None and not (isinstance(n, dict) and n.get("title") and n.get("urgency") in ("low", "high")):
            errors.append("decision.notify must be null or {title, body, urgency: low|high}")
    src = os.path.join(out_dir, "sources.json")
    if os.path.exists(src):
        s = read_json(src, None)
        if not isinstance(s, list) or not all(isinstance(x, dict) and x.get("id") and x.get("url") and x.get("type") for x in s):
            errors.append("out/sources.json must be a list of {id, url, type, ...}")
        elif len(s) > 40: errors.append("out/sources.json has more than 40 sources")
    return errors

# ------------------------------------------------------------------ the agent
def describe(tool, inp):
    if tool == "WebFetch": return "read   " + inp.get("url", "")
    if tool == "WebSearch": return "search " + inp.get("query", "")
    if tool in ("Read", "Write", "Edit"): return tool.lower().ljust(6) + " " + os.path.basename(inp.get("file_path", ""))
    if tool == "ToolSearch": return None
    return tool + " " + json.dumps(inp)[:80]

def run_agent(run_dir, prompt, model, resume=None):
    """Run claude -p, stream progress, return (result_event or None, session_id, timed_out)."""
    system = open(os.path.join(ASSETS, "system.md")).read()
    # Not --restricted: it also strips WebFetch. Shell tools are blocked explicitly instead.
    cmd = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
           "--allowedTools", TOOLS, "--disallowedTools", BLOCKED, "--append-system-prompt", system]
    if model: cmd += ["--model", model]
    if resume: cmd += ["--resume", resume]
    log = open(os.path.join(run_dir, "stream.jsonl"), "a")
    # The editor runs without herdr's pane identity: the shared Claude hook would otherwise claim the
    # pane's agent session as herdr:claude and herdr would drop this runner's own herdr:news reports.
    env = {k: v for k, v in os.environ.items() if k not in ("HERDR_PANE_ID", "HERDR_TAB_ID", "HERDR_WORKSPACE_ID")}
    proc = subprocess.Popen(cmd, cwd=run_dir, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            start_new_session=True, text=True, bufsize=1, env=env)
    deadline = time.time() + WATCHDOG_S
    result, session, timed_out, reads = None, resume, False, 0
    try:
        while True:
            if time.time() > deadline:
                timed_out = True; os.killpg(proc.pid, signal.SIGTERM); break
            r, _, _ = select.select([proc.stdout], [], [], 5)
            if not r:
                if proc.poll() is not None: break
                continue
            line = proc.stdout.readline()
            if not line:
                if proc.poll() is not None: break
                continue
            log.write(line)
            try: e = json.loads(line)
            except ValueError:
                say(line.rstrip()[:160], "2"); continue
            t = e.get("type")
            if t == "system" and e.get("subtype") == "init":
                session = e.get("session_id"); say("editor started (%s, session %s)" % (e.get("model"), session[:8]), "1")
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
    finally:
        try: proc.wait(timeout=10)
        except subprocess.TimeoutExpired: os.killpg(proc.pid, signal.SIGKILL)
        log.close()
    return result, session, timed_out

# ------------------------------------------------------------------ publish
def since_lines(page, prev):
    if not prev: return ["First edition."]
    old = {i["url"] for s in prev.get("sections", []) for i in s["items"]} | {(prev.get("lead") or {}).get("url")}
    new = {i["url"] for s in page.get("sections", []) for i in s["items"]} | {page["lead"]["url"]}
    return ["%d new stories, %d dropped." % (len(new - old), len(old - new))]

def publish(home, run_dir, page, decision, trigger, anchors_doc, started):
    idx = editions_index(home)
    n = (idx["editions"][-1]["edition"] if idx["editions"] else 0) + 1
    at = now_utc()
    prev = read_json(os.path.join(home, "page.json"), None)
    items = [page["lead"]] + [i for s in page["sections"] for i in s["items"]]
    page.update({"version": 1, "edition": n, "updated": iso(at), "trigger": trigger, "stub": False,
                 "next_run": None,
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
    new_sources = os.path.join(run_dir, "out", "sources.json")
    if os.path.exists(new_sources): shutil.copy(new_sources, os.path.join(home, "sources.json"))
    return n

# ------------------------------------------------------------------ main
def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--home", required=True)
    ap.add_argument("--trigger", default="manual", choices=["manual", "scheduled"])
    ap.add_argument("--model")
    ap.add_argument("--dry-run", action="store_true", help="prepare inputs and fetch anchors only")
    ap.add_argument("--no-view", action="store_true")
    a = ap.parse_args(argv)
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
    result, session, timed_out = run_agent(run_dir, prompt, a.model)
    for attempt in range(2):
        if result:
            total_cost += result.get("total_cost_usd") or 0; turns += result.get("num_turns") or 0
        record.update(cost_usd=round(total_cost, 4), turns=turns)
        if timed_out:
            record["errors"] = ["watchdog: no result after %d minutes" % (WATCHDOG_S // 60)]
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
        result, session, timed_out = run_agent(run_dir, fix, a.model, resume=session)

    page = read_json(os.path.join(run_dir, "out", "page.json"), None)
    decision = read_json(os.path.join(run_dir, "out", "decision.json"), {})
    record["decision"] = decision
    record["edition"] = publish(home, run_dir, page, decision, a.trigger, anchors_doc, started)
    code = finish("ok", 0)
    if not a.no_view and sys.stdout.isatty():
        viewer = os.path.join(ASSETS, "viewer.py")
        os.execvp(sys.executable, [sys.executable, viewer, os.path.join(home, "page.json")])
    return code


if __name__ == "__main__":
    sys.exit(main())
