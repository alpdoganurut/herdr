#!/usr/bin/env python3
"""Checks for the news runner asset (src/integration/assets/news/news_run.py): the story
validator's optional `changed` / `what_changed` fields and the first-seen index it keeps
beside the editions (built once from the existing editions, never moving a story later).
Run: python3 -m unittest scripts/test_news_run.py"""
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSET = os.path.join(ROOT, "src", "integration", "assets", "news", "news_run.py")


def load_runner():
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("news_run", ASSET)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def item(**extra):
    base = {"head": "A lab ships a model", "text": " ".join(["word"] * 30), "url": "https://ex.test/a",
            "source": "Example", "time": "2026-10-01T10:00:00+00:00", "read": True}
    base.update(extra)
    return base


class Validator(unittest.TestCase):
    def setUp(self):
        self.r = load_runner()

    def errors_for(self, it):
        errors = []
        self.r.check_item("sections[0].items[0]", it, {}, errors)
        return errors

    def test_the_optional_change_fields_are_type_checked(self):
        self.assertEqual(self.errors_for(item()), [])
        self.assertEqual(self.errors_for(item(changed=True, what_changed="A second detail was added.")), [])
        self.assertEqual(self.errors_for(item(changed=False)), [])
        self.assertTrue(any("changed must be true or false" in e for e in self.errors_for(item(changed="yes"))))
        self.assertTrue(any("what_changed must be a string" in e for e in self.errors_for(item(what_changed=3))))
        self.assertTrue(any("control characters" in e for e in self.errors_for(item(what_changed="bad\x1b[31m"))))
        long = " ".join(["w"] * 31)
        self.assertTrue(any("what_changed is 31 words" in e for e in self.errors_for(item(what_changed=long))))


class FirstSeen(unittest.TestCase):
    def setUp(self):
        self.r = load_runner()
        self.home = tempfile.mkdtemp(prefix="herdr-news-run-")
        os.makedirs(os.path.join(self.home, "editions", "2026-10-01"))

    def tearDown(self):
        shutil.rmtree(self.home, ignore_errors=True)

    def edition(self, n, urls, at):
        rel = "2026-10-01/%02d00-e%04d.json" % (8 + n, n)
        page = {"edition": n, "lead": {"head": "Lead %d" % n, "url": urls[0]},
                "sections": [{"title": "T", "items": [{"head": "H", "url": u} for u in urls[1:]]}]}
        with open(os.path.join(self.home, "editions", rel), "w") as f: json.dump(page, f)
        return {"edition": n, "path": rel, "at": at, "day": "2026-10-01"}

    def test_backfills_once_from_the_index_then_only_adds(self):
        r = self.r
        idx = {"version": 1, "editions": [
            self.edition(1, ["https://ex.test/lead", "https://ex.test/a"], "2026-10-01T05:00:00+00:00"),
            self.edition(2, ["https://ex.test/lead", "https://ex.test/b"], "2026-10-01T06:00:00+00:00"),
        ]}
        items3 = [{"head": "Lead", "url": "https://ex.test/lead"}, {"head": "B", "url": "https://ex.test/b"}, {"head": "C", "url": "https://ex.test/c"}]
        doc = r.first_seen_index(self.home, idx, 3, "2026-10-01T07:00:00+00:00", items3)
        s = doc["stories"]
        self.assertEqual(doc["version"], 1)
        self.assertEqual(s["https://ex.test/lead"]["edition"], 1, "backfilled from the first edition")
        self.assertEqual(s["https://ex.test/a"]["edition"], 1)
        self.assertEqual(s["https://ex.test/b"]["edition"], 2)
        self.assertEqual(s["https://ex.test/c"], {"edition": 3, "at": "2026-10-01T07:00:00+00:00"})
        # written, the next edition only adds: an older first-seen never moves
        with open(os.path.join(self.home, "first_seen.json"), "w") as f: json.dump(doc, f)
        idx["editions"].append(self.edition(3, ["https://ex.test/lead", "https://ex.test/b", "https://ex.test/c"], "2026-10-01T07:00:00+00:00"))
        items4 = [{"head": "Lead", "url": "https://ex.test/lead"}, {"head": "C", "url": "https://ex.test/c"}, {"head": "D", "url": "https://ex.test/d"}]
        doc2 = r.first_seen_index(self.home, idx, 4, "2026-10-01T08:00:00+00:00", items4)
        self.assertEqual(doc2["stories"]["https://ex.test/c"]["edition"], 3)
        self.assertEqual(doc2["stories"]["https://ex.test/d"]["edition"], 4)
        self.assertEqual(doc2["stories"]["https://ex.test/a"]["edition"], 1, "a dropped story keeps its record")

    def test_a_story_without_a_url_is_keyed_by_its_headline(self):
        r = self.r
        self.assertEqual(r.story_key({"head": "  Two   Words "}), "two words")
        self.assertEqual(r.story_key({"url": "https://ex.test/x", "head": "ignored"}), "https://ex.test/x")


class Asset(unittest.TestCase):
    def test_compiles_without_leaving_pycache(self):
        import py_compile
        with tempfile.TemporaryDirectory() as tmp:
            py_compile.compile(ASSET, cfile=os.path.join(tmp, "news_run.pyc"), doraise=True)
        self.assertFalse(os.path.exists(os.path.join(os.path.dirname(ASSET), "__pycache__")))


if __name__ == "__main__":
    unittest.main()
