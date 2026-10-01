#!/usr/bin/env python3
"""Checks for the news viewer asset (src/integration/assets/news/viewer.py): the marker logic
(new / updated / first-seen ranks), the two-leaf dealing rule (a section never splits, each
section goes to the shorter leaf), the word diff, and a pty run of the pinned viewer on a
small fixture home — keys n, u, down/up, then herdr's quit sequence — draining its output
and checking it leaves the terminal in order. Also: the asset compiles and leaves no
__pycache__ behind. Run: python3 -m unittest scripts/test_news_viewer.py"""
import importlib.util
import json
import os
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from datetime import datetime, timedelta, timezone

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSET = os.path.join(ROOT, "src", "integration", "assets", "news", "viewer.py")
ENV = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")


def load_viewer():
    spec = importlib.util.spec_from_file_location("news_viewer", ASSET)
    module = importlib.util.module_from_spec(spec)
    sys.dont_write_bytecode = True
    spec.loader.exec_module(module)
    module.set_theme("Dusk")
    return module


def story(url, head, text, time_iso, **extra):
    item = {"head": head, "text": text, "url": url, "source": "Example", "time": time_iso, "read": True}
    item.update(extra)
    return item


def page(edition, at, lead, sections):
    return {"version": 1, "edition": edition, "updated": at.isoformat(timespec="seconds"), "trigger": "scheduled",
            "stub": False, "next_run": None, "stats": {"anchor_items": 9, "sources": 3, "read": 4},
            "since_last_run": ["a line"], "lead": lead, "sections": sections}


def fixture_home(root):
    """Three editions: e1 (yesterday), e2 (today 08:00, the one 'last read'), e3 (today 13:00) with
    one new story, one updated story (text changed + what_changed), carried-over ones."""
    now = datetime(2026, 10, 1, 13, 0, tzinfo=timezone.utc)
    t1, t2, t3 = now - timedelta(days=1), now - timedelta(hours=5), now
    base_text = "The lab shipped a model with a longer context window and a lower price for developers today."
    lead = story("https://ex.test/lead", "Lab ships model", base_text, t1.isoformat(timespec="seconds"), standfirst="A standfirst of enough words to pass as a lead for the viewer in these checks.")
    a = story("https://ex.test/a", "Story A about tooling", "Story A text says one thing about tooling and nothing else right now.", t1.isoformat(timespec="seconds"))
    b = story("https://ex.test/b", "Story B about policy", "Story B text covers a policy move with one concrete detail in it.", t2.isoformat(timespec="seconds"))
    c = story("https://ex.test/c", "Story C brand new", "Story C is new in the third edition and carries a fresh detail.", t3.isoformat(timespec="seconds"))
    b2 = dict(b, text="Story B text covers a policy move with two concrete details in it now.", changed=True, what_changed="A second detail was added.")
    e1 = page(1, t1, lead, [{"title": "Tools", "items": [a]}])
    e2 = page(2, t2, lead, [{"title": "Tools", "items": [a]}, {"title": "Policy", "items": [b]}])
    e3 = page(3, t3, lead, [{"title": "Tools", "items": [a, c]}, {"title": "Policy", "items": [b2]}])
    eds = []
    for n, (pg, at) in enumerate([(e1, t1), (e2, t2), (e3, t3)], start=1):
        day = at.strftime("%Y-%m-%d")
        rel = "%s/%s-e%04d.json" % (day, at.strftime("%H%M"), n)
        os.makedirs(os.path.join(root, "editions", day), exist_ok=True)
        with open(os.path.join(root, "editions", rel), "w") as f: json.dump(pg, f)
        eds.append({"edition": n, "path": rel, "at": at.isoformat(timespec="seconds"), "day": day, "trigger": "scheduled", "stories": 2, "changed": True})
    with open(os.path.join(root, "editions", "index.json"), "w") as f: json.dump({"version": 1, "editions": eds}, f)
    with open(os.path.join(root, "page.json"), "w") as f: json.dump(e3, f)
    with open(os.path.join(root, "read.json"), "w") as f: json.dump({"version": 1, "last_read_edition": 2}, f)
    with open(os.path.join(root, "first_seen.json"), "w") as f:
        json.dump({"version": 1, "stories": {
            "https://ex.test/lead": {"edition": 1, "at": eds[0]["at"]},
            "https://ex.test/a": {"edition": 1, "at": eds[0]["at"]},
            "https://ex.test/b": {"edition": 2, "at": eds[1]["at"]},
            "https://ex.test/c": {"edition": 3, "at": eds[2]["at"]}}}, f)
    return root


class MarkerLogic(unittest.TestCase):
    def setUp(self):
        self.v = load_viewer()
        self.dir = tempfile.mkdtemp(prefix="herdr-news-viewer-")
        fixture_home(self.dir)
        v = self.v
        v.HIST["home"] = self.dir
        v.HIST["dir"], v.HIST["eds"] = v.load_history(os.path.join(self.dir, "page.json"))
        v.PAGES.clear()

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def test_latest_edition_compares_against_the_last_read_one(self):
        v = self.v
        page3 = v.open_edition(2)
        self.assertEqual(v.baseline_index(2), 1, "edition 3 vs the last read edition 2")
        states = {k: m["state"] for k, m in v.MK.items()}
        self.assertEqual(states["https://ex.test/c"], "new")
        self.assertEqual(states["https://ex.test/b"], "upd")
        self.assertIsNone(states["https://ex.test/a"])
        self.assertEqual((v.META["new"], v.META["upd"]), (1, 1))
        self.assertEqual(v.MK["https://ex.test/b"]["note"], "A second detail was added.")
        self.assertEqual(v.MK["https://ex.test/a"]["rank"], 2, "first seen two editions ago (from first_seen.json)")
        self.assertEqual(v.MK["https://ex.test/c"]["rank"], 0)
        self.assertEqual(v.META["since"].strftime("%H:%M"), v.local(v.HIST["eds"][1]["at"]).strftime("%H:%M"))
        # the viewer tells herdr what it shows
        with open(os.path.join(self.dir, "viewer-state.json")) as f:
            self.assertEqual(json.load(f)["showing"], 3)
        # the edition already read (or older) compares against the one before it
        self.assertEqual(v.baseline_index(1), 0)
        self.assertIsNone(v.baseline_index(0))
        v.open_edition(1)
        self.assertEqual(v.MK["https://ex.test/b"]["state"], "new", "edition 2 vs edition 1")
        self.assertEqual(page3["edition"], 3)

    def test_without_first_seen_json_the_history_scan_ranks_stories(self):
        v = self.v
        os.remove(os.path.join(self.dir, "first_seen.json"))
        v.open_edition(2)
        self.assertEqual(v.MK["https://ex.test/a"]["rank"], 2)
        self.assertEqual(v.MK["https://ex.test/b"]["rank"], 1)

    def test_a_new_story_keeps_its_real_first_seen_time(self):
        # last read = edition 1: b (first seen in edition 2) and c (edition 3) are both new on edition 3,
        # but b's chip and rank come from the runner's log, not from the edition on screen
        v = self.v
        with open(os.path.join(self.dir, "read.json"), "w") as f: json.dump({"version": 1, "last_read_edition": 1}, f)
        v.open_edition(2)
        eds = v.HIST["eds"]
        b, c = v.MK["https://ex.test/b"], v.MK["https://ex.test/c"]
        self.assertEqual((b["state"], c["state"]), ("new", "new"))
        self.assertEqual(b["first"], v.local(eds[1]["at"]))
        self.assertEqual(b["rank"], 1)
        self.assertEqual(c["first"], v.local(eds[2]["at"]))
        self.assertEqual(c["rank"], 0)
        # without a first-seen record the shown edition stands in
        os.remove(os.path.join(self.dir, "first_seen.json"))
        v.open_edition(2)
        self.assertEqual(v.MK["https://ex.test/b"]["rank"], 1, "the history scan also knows b")

    def test_open_edition_loads_only_what_it_needs_and_the_page_cache_is_bounded(self):
        v = self.v
        eds = v.HIST["eds"]
        path = lambda k: os.path.join(v.HIST["dir"], eds[k]["path"])
        v.PAGES.clear()
        v.open_edition(2)
        self.assertEqual(set(v.PAGES), {path(2), path(1)}, "the shown and the baseline page only (first_seen.json ranks the rest)")
        os.remove(os.path.join(self.dir, "first_seen.json"))
        v.PAGES.clear()
        v.open_edition(2)
        self.assertEqual(set(v.PAGES), {path(0), path(1), path(2)}, "no log: every edition up to the shown one")
        saved = v.PAGES_MAX
        try:
            v.PAGES_MAX = 2
            v.PAGES.clear()
            v.open_edition(2)
            self.assertLessEqual(len(v.PAGES), 2, "the cache never grows past PAGES_MAX")
            self.assertEqual(v.MK["https://ex.test/a"]["rank"], 2, "the scan still ranked every story")
        finally:
            v.PAGES_MAX = saved

    def test_chips_and_buckets_follow_the_first_seen_time(self):
        v = self.v
        v.open_edition(2)
        labels = [lab for lab, _, _ in v.META["buckets"]]
        self.assertEqual(labels[0], v.META["at"].strftime("%H:%M"), "newest bucket first")
        self.assertIn("yest.", labels)
        self.assertEqual(v.chip_label(None), "older")

    def test_word_diff_strikes_old_words_and_golds_new_ones(self):
        v = self.v
        segs = v.word_diff("one concrete detail in it", "two concrete details in it now", 60)
        texts = [t for t, _, _ in segs]
        joined = " ".join(texts)
        self.assertIn("one", joined)
        self.assertIn("two", joined)
        styles = [st for _, st, _ in segs]
        self.assertTrue(any(";9" in st for st in styles), "a struck-through segment")
        self.assertEqual(v.word_diff("same words", "same words", 40), [])

    def test_new_only_filter_hides_unmarked_stories_and_n_jumps_between_marks(self):
        v = self.v
        page3 = v.open_edition(2)
        rows, secs, targets, _ = v.build(page3, 120, -1)
        self.assertEqual(len(targets), 4, "lead + a + c + b")
        self.assertEqual(v.STATES, [None, None, "new", "upd"])
        self.assertEqual(v.jump_new(-1, targets, 1), 2)
        self.assertEqual(v.jump_new(2, targets, 1), 3)
        self.assertEqual(v.jump_new(3, targets, 1), 2, "wraps")
        self.assertEqual(v.jump_new(-1, targets, -1), 3)
        v.FILTER[0] = True
        try:
            rows, secs, targets, _ = v.build(page3, 120, -1)
            self.assertEqual(len(targets), 2)
            text = "".join(t for r in rows for t, _, _ in r)
            self.assertIn("new only  ·  2 older stories folded", text)
        finally:
            v.FILTER[0] = False

    def test_the_title_page_and_rules_carry_the_counts(self):
        v = self.v
        page3 = v.open_edition(2)
        rows, _, _, _ = v.build(page3, 120, -1)
        text = "\n".join("".join(t for t, _, _ in r) for r in rows)
        self.assertIn("● 1 new   ◑ 1 updated   since you read the", text)
        self.assertIn("first seen", text)
        self.assertIn("●1 new", text)
        self.assertIn("◑1 upd", text)
        self.assertIn("↻ revised", text)
        self.assertIn("A second detail was added.", text)
        self.assertIn("Δ", text)


class Dealing(unittest.TestCase):
    def setUp(self):
        self.v = load_viewer()

    def blocks(self, sizes):
        """Groups of (section opener + stories) with the given story-row sizes; a lead first."""
        v = self.v
        out = [v.block([("  ", [v.seg("lead")], None)] * 3, sec=-1)]
        for n, size in enumerate(sizes):
            out.append(v.block([("  ", [v.seg("rule")], None)], sec=n, keep=True))
            out.append(v.block([("  ", [v.seg("x")], None)] * size, sec=n))
        return out

    def test_sections_never_split_and_go_to_the_shorter_leaf(self):
        v = self.v
        left, right = v.deal_sections(self.blocks([10, 4, 4, 2]))
        # the lead opens the left leaf; sections are dealt to the shorter side, ties left
        def secs(bs): return [b["sec"] for b in bs if b["keep"]]
        # lead(3) opens left; S0(11) -> right (3 > 0); S1(5) -> left (3 <= 11) = 8;
        # S2(5) -> left (8 <= 11) = 13; S3(3) -> right (13 > 11)
        self.assertEqual(secs(left), [1, 2])
        self.assertEqual(secs(right), [0, 3])
        for leaf in (left, right):
            for k, b in enumerate(leaf):
                if b["keep"]:
                    self.assertIs(leaf[k + 1]["sec"], b["sec"], "a section's stories follow its opener on the same leaf")
        # every section lands on exactly one leaf
        all_secs = sorted(secs(left) + secs(right))
        self.assertEqual(all_secs, [0, 1, 2, 3])

    def test_a_spread_lays_whole_sections_per_leaf(self):
        v = self.v
        dir_ = tempfile.mkdtemp(prefix="herdr-news-viewer-")
        try:
            fixture_home(dir_)
            v.HIST["home"] = dir_
            v.HIST["dir"], v.HIST["eds"] = v.load_history(os.path.join(dir_, "page.json"))
            v.PAGES.clear()
            page3 = v.open_edition(2)
            rows, secs, targets, _ = v.build(page3, 170, -1)
            self.assertEqual(v.geometry(170)["mode"], "spread")
            # the "Policy" rule and its only story sit on the same side of the fold
            def side(needle):
                for r in rows:
                    text = "".join(t for t, _, _ in r)
                    if needle in text:
                        fold = text.find("┆")
                        self.assertGreaterEqual(fold, 0, "a spread row has a fold")
                        return "left" if text.find(needle) < fold else "right"
                self.fail("no row with %r" % needle)
            self.assertEqual(side("P O L I C Y"), side("Story B about policy"))
            self.assertEqual(side("T O O L S"), side("Story A about tooling"))
        finally:
            shutil.rmtree(dir_, ignore_errors=True)


class Asset(unittest.TestCase):
    def test_compiles_without_leaving_pycache(self):
        import py_compile
        here = os.path.dirname(ASSET)
        with tempfile.TemporaryDirectory() as tmp:
            # the bytecode goes to a temp file: the asset directory stays clean
            py_compile.compile(ASSET, cfile=os.path.join(tmp, "viewer.pyc"), doraise=True)
        self.assertFalse(os.path.exists(os.path.join(here, "__pycache__")), "no __pycache__ beside the asset")

    def test_pinned_viewer_runs_in_a_pty_and_ends_on_herdrs_quit_sequence(self):
        dir_ = tempfile.mkdtemp(prefix="herdr-news-viewer-pty-")
        try:
            fixture_home(dir_)
            pid, fd = pty.fork()
            if pid == 0:  # child
                os.environ.update(ENV)
                os.environ["COLUMNS"], os.environ["LINES"] = "120", "40"
                os.execv(sys.executable, [sys.executable, ASSET, os.path.join(dir_, "page.json"), "--pinned"])
            import fcntl, struct, termios
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
            out = bytearray()

            def drain(seconds):
                end = time.time() + seconds
                while time.time() < end:
                    r, _, _ = select.select([fd], [], [], 0.05)
                    if fd in r:
                        try:
                            chunk = os.read(fd, 65536)
                        except OSError:
                            return False
                        if not chunk:
                            return False
                        out.extend(chunk)
                return True

            self.assertTrue(drain(1.5))
            first = out.decode(errors="replace")
            self.assertIn("\x1b[?1049h", first, "alternate screen")
            self.assertIn("since you read", first)
            for key in (b"n", b"n", b"u", b"\x1b[B", b"\x1b[A", b"j", b"G", b"g"):
                os.write(fd, key)
                drain(0.3)
            os.write(fd, b"q")           # pinned: q does nothing
            drain(0.3)
            os.write(fd, b"\x03")        # Ctrl-C arrives as a byte; nothing happens
            drain(0.3)
            _, status = os.waitpid(pid, os.WNOHANG)
            self.assertEqual(status, 0, "still running after q and Ctrl-C")
            os.write(fd, b"\x1b[9999~")  # herdr's quit sequence
            deadline = time.time() + 5
            ended = None
            while time.time() < deadline:
                drain(0.1)
                p, status = os.waitpid(pid, os.WNOHANG)
                if p == pid:
                    ended = status
                    break
            self.assertIsNotNone(ended, "the viewer exits on HERDR_QUIT")
            self.assertEqual(os.waitstatus_to_exitcode(ended), 0)
            tail = out.decode(errors="replace")
            self.assertIn("\x1b[?1049l", tail, "main screen restored")
            self.assertIn("\x1b[?25h", tail, "cursor restored")
            with open(os.path.join(dir_, "viewer-state.json")) as f:
                self.assertIn(json.load(f)["showing"], (2, 3))
        finally:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
            shutil.rmtree(dir_, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
