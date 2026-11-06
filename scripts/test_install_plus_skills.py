"""Tests for scripts/install-plus-skills.sh (the herdr+ Claude skills installer)."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "install-plus-skills.sh"
SKILLS = REPO / "docs" / "fork" / "skills"
NAMES = ["plus:notes", "plus:notify", "plus:setup", "plus:status", "plus:team-build"]


def run(dest: Path, *args: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if k != "CLAUDE_CONFIG_DIR"}
    return subprocess.run(
        ["bash", str(SCRIPT), "--dest", str(dest), *args],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )


class PackagedSkillsTest(unittest.TestCase):
    def test_every_packaged_skill_declares_its_installed_name(self) -> None:
        dirs = sorted(p.name for p in SKILLS.iterdir() if p.is_dir())
        self.assertEqual(dirs, ["plus-notes", "plus-notify", "plus-setup", "plus-status", "plus-team-build"])
        for d in dirs:
            text = (SKILLS / d / "SKILL.md").read_text(encoding="utf-8")
            self.assertIn(f"\nname: plus:{d[len('plus-'):]}\n", text)

    def test_packaged_skills_carry_no_owner_specific_or_stale_content(self) -> None:
        for path in SKILLS.rglob("*"):
            self.assertNotIn(".bak", path.name, path)
            self.assertNotIn(":", path.name, path)
            if path.is_file():
                text = path.read_text(encoding="utf-8")
                for needle in ("/Users/", "alpdogan", "agents_checkpoint", "search it", "leap"):
                    self.assertNotIn(needle, text.lower() if needle == "leap" else text, f"{needle} in {path}")

    def test_deadline_wrapper_is_executable_in_the_repo(self) -> None:
        wrapper = SKILLS / "plus-team-build" / "templates" / "run-with-deadline.sh"
        self.assertTrue(os.access(wrapper, os.X_OK))


class InstallerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.dest = self.root / "claude" / "skills"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_fresh_install_copies_all_skills_with_exec_bit(self) -> None:
        proc = run(self.dest)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        for name in NAMES:
            self.assertTrue((self.dest / name / "SKILL.md").is_file(), name)
        wrapper = self.dest / "plus:team-build" / "templates" / "run-with-deadline.sh"
        self.assertTrue(os.access(wrapper, os.X_OK))
        self.assertIn("installed 5, unchanged 0, backed up 0", proc.stdout)
        self.assertEqual(sorted(p.name for p in self.dest.iterdir()), NAMES)

    def test_second_run_is_a_no_op(self) -> None:
        self.assertEqual(run(self.dest).returncode, 0)
        proc = run(self.dest)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("installed 0, unchanged 5, backed up 0", proc.stdout)
        self.assertFalse((self.root / "claude" / "skills-backup").exists())

    def test_changed_skill_is_backed_up_outside_the_skills_folder(self) -> None:
        self.assertEqual(run(self.dest).returncode, 0)
        edited = self.dest / "plus:notes" / "SKILL.md"
        edited.write_text("local edit\n", encoding="utf-8")
        unrelated = self.dest / "my-skill"
        unrelated.mkdir()
        (unrelated / "SKILL.md").write_text("mine\n", encoding="utf-8")

        proc = run(self.dest)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("installed 1, unchanged 4, backed up 1", proc.stdout)
        backups = list((self.root / "claude" / "skills-backup").iterdir())
        self.assertEqual(len(backups), 1)
        self.assertTrue(backups[0].name.startswith("plus:notes."))
        self.assertEqual((backups[0] / "SKILL.md").read_text(encoding="utf-8"), "local edit\n")
        self.assertEqual(
            edited.read_bytes(), (SKILLS / "plus-notes" / "SKILL.md").read_bytes()
        )
        self.assertEqual((unrelated / "SKILL.md").read_text(encoding="utf-8"), "mine\n")

    def test_dry_run_touches_nothing(self) -> None:
        proc = run(self.dest, "--dry-run")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("installed 5", proc.stdout)
        self.assertFalse(self.dest.exists())

    def test_unknown_argument_fails(self) -> None:
        proc = run(self.dest, "--bogus")
        self.assertEqual(proc.returncode, 2)


if __name__ == "__main__":
    unittest.main()
