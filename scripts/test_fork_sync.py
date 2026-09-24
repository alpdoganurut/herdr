"""Scratch-repo tests for scripts/fork_sync.sh.

Every test builds a fake upstream, a fake `fork` checkout (the "main
checkout"), and a bare `mine` remote in a temp dir, then drives the script's
subcommands. The cargo gate is replaced through FORK_SYNC_GATE_CMD and the
install through FORK_SYNC_BUILT_BINARY / FORK_SYNC_CODESIGN, so the suite
runs in seconds and never touches the real repo, ~/.local/bin or a server.

Run: python3 scripts/test_fork_sync.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
SCRIPT = SCRIPTS / "fork_sync.sh"
sys.path.insert(0, str(SCRIPTS))

import fork_sync_lib  # noqa: E402

# Same headings and line formats as the real FORK.md (a small subset).
FORK_MD = """# FORK.md — herdr fork manifest

Section 1 drift check: `git diff --diff-filter=A --name-only $(git merge-base origin/master fork) fork -- . ':(exclude)docs/next/api'` must print exactly the section 1 list.

## 1. Owned files (added by the fork; upstream R/D/T on any = deny)
FORK.md
src/app/agent_suspend.rs

## 2. Owned fields on upstream structs (E0063 in upstream-authored literals: insert the default)
| struct | field | default |
| PersistedAgentSession | transcript_path | None |
| App | backup_agent_transcripts | config.session.backup_agent_transcripts |
| App | agent_transcript_backup_last | None |

## 3. Removed or re-signatured upstream symbols (E0425/E0061 at a new upstream call site = deny)
app::api_helpers::pane_agent_status(state, seen) -> removed

## 4. Owned enum variants (append last; E0004 in upstream match = deny)
AgentStatus::Suspended   [src/api/schema/common.rs, last after Unknown; append-closed, deny on conflict]

## 5. Owned API methods and digests
agent.suspend: fork-defined.

## 6. Config keys (cross-checked by scripts/config_reference_check.py)
ui.sidebar_layout

## 7. Per-file merge rules
docs/next/CHANGELOG.md  take-theirs: fork entries live in section 11
skills/herdr/SKILL.md  take-theirs+reapply: re-apply the fork's hunks with git apply --3way
src/config/model.rs  additive: upstream first, fork lines directly after each clear_pane line
src/server/client_commands.rs  section-5
src/api/schema/common.rs  deny: AgentStatus is append-closed
*  deny: anything that is not a structural additive conflict

## 8. Extended surfaces (upstream touch forces human review in the report, even when green)
src/protocol/wire.rs  mid-logic: deserialize_client_shell_agent_status
src/client/shell/input.rs  mid-logic: push_pane_key is the only lock point

## 9. Identifier watch-list (any hit in the incoming upstream diff = deny "upstream collision")
agent.suspend
Suspended
"suspended"
transcript_path:

## 10. Fork smoke tests (run by name in the gate)
server::headless::tests::fork_smoke::suspended_status_reaches_the_client_shell_snapshot

## 11. Fork changelog (moved out of docs/next/CHANGELOG.md)
### Added
- herdr agent suspend

## 12. Install rule
Compare PROTOCOL_VERSION and ENDPOINT_PROTOCOL_GENERATION. Never restart the server.
"""

SHARED = "".join(f"shared line {i}\n" for i in range(1, 31))


def git(cwd: Path, *args: str) -> str:
    proc = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True)
    if proc.returncode != 0:
        raise AssertionError(f"git {' '.join(args)} failed: {proc.stderr}")
    return proc.stdout.strip()


def write(root: Path, files: dict[str, str]) -> None:
    for rel, content in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)


class ForkSyncCase(unittest.TestCase):
    _template: tempfile.TemporaryDirectory | None = None

    @classmethod
    def _build_template(cls) -> Path:
        """Build upstream / fork checkout / bare mine once; tests copy it."""
        if ForkSyncCase._template is None:
            ForkSyncCase._template = tempfile.TemporaryDirectory(prefix="fork-sync-template-")
            root = Path(ForkSyncCase._template.name).resolve()
            up, main, mine = root / "up", root / "main", root / "mine.git"
            subprocess.run(["git", "init", "-q", "-b", "master", str(up)], check=True)
            cls._identity(up)
            write(
                up,
                {
                    ".gitignore": "/.local/\n",
                    "Cargo.toml": '[package]\nname = "herdr"\nversion = "0.9.1"\n',
                    "src/protocol/wire.rs": "pub const PROTOCOL_VERSION: u32 = 22;\n",
                    "src/protocol/endpoint.rs": "pub const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;\n",
                    "src/shared.rs": SHARED,
                    "src/config/model.rs": "fn base_one() {}\nfn base_two() {}\n",
                    "src/client/view.rs": "fn view() {}\n",
                    "src/server/core.rs": "fn core() {}\n",
                    "docs/notes.md": "notes\n",
                },
            )
            cls.commit(up, "base")
            subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(up), str(main)], check=True)
            cls._identity(main)
            git(main, "checkout", "-q", "-b", "fork")
            write(
                main,
                {
                    "FORK.md": FORK_MD,
                    "src/app/agent_suspend.rs": "fn suspend() {}\n",
                    "src/shared.rs": SHARED.replace("shared line 5\n", "shared line 5 (fork)\n"),
                    "src/config/model.rs": "fn base_one() {}\nfn base_two() {}\nfn fork_added() {}\n",
                },
            )
            cls.commit(main, "fork: suspend")
            subprocess.run(["git", "init", "-q", "--bare", str(mine)], check=True)
            git(main, "remote", "add", "mine", str(mine))
            git(main, "push", "-q", "mine", "fork")
            git(main, "fetch", "-q", "mine")
        return Path(ForkSyncCase._template.name).resolve()

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="fork-sync-test-")
        self.root = Path(self._tmp.name).resolve()
        self.up = self.root / "up"
        self.main = self.root / "main"
        self.mine = self.root / "mine.git"
        self.home = self.root / "home"
        self.bin_dir = self.root / "bin"
        self.state_dir = self.root / "state"
        self.worktree = self.root / "worktrees" / "fork-sync"
        self.built = self.root / "built" / "herdr"
        template = self._build_template()
        for name in ("up", "main", "mine.git"):
            shutil.copytree(template / name, self.root / name, symlinks=True)
        git(self.main, "remote", "set-url", "origin", str(self.up))
        git(self.main, "remote", "set-url", "mine", str(self.mine))
        self.home.mkdir()
        write(self.root, {"built/herdr": "#!/bin/sh\necho new build\n"})
        self.f0 = git(self.main, "rev-parse", "fork")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    # -- helpers -----------------------------------------------------------

    @staticmethod
    def _identity(repo: Path) -> None:
        git(repo, "config", "user.name", "Fork Sync Test")
        git(repo, "config", "user.email", "fork-sync@example.invalid")
        git(repo, "config", "commit.gpgsign", "false")

    @staticmethod
    def commit(repo: Path, message: str) -> str:
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", message)
        return git(repo, "rev-parse", "HEAD")

    def upstream(self, message: str, files: dict[str, str] | None = None, delete=(), rename=()) -> str:
        write(self.up, files or {})
        for rel in delete:
            git(self.up, "rm", "-q", rel)
        for old, new in rename:
            git(self.up, "mv", old, new)
        return self.commit(self.up, message)

    def env(self, **extra: str) -> dict[str, str]:
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("HERDR_") and not key.startswith("FORK_SYNC_") and not key.startswith("GIT_")
        }
        env.update(
            {
                "HOME": str(self.home),
                "GIT_CONFIG_NOSYSTEM": "1",
                "FORK_SYNC_WORKTREE": str(self.worktree),
                "FORK_SYNC_STATE_DIR": str(self.state_dir),
                "FORK_SYNC_BIN_DIR": str(self.bin_dir),
                "FORK_SYNC_BUILT_BINARY": str(self.built),
                "FORK_SYNC_CODESIGN": "true",
                "FORK_SYNC_GATE_CMD": "true",
            }
        )
        env.update(extra)
        return env

    def sync(self, *args: str, **extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(SCRIPT), *args],
            cwd=self.main,
            env=self.env(**extra),
            capture_output=True,
            text=True,
        )

    def assertExit(self, proc: subprocess.CompletedProcess[str], code: int) -> None:
        self.assertEqual(
            proc.returncode, code, f"expected exit {code}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )

    def state(self) -> dict:
        return json.loads((self.main / ".local/fork-sync/state.json").read_text())

    def report(self) -> str:
        return Path(self.state()["report"]).read_text()

    def denial_kinds(self) -> list[str]:
        return [d["kind"] for d in self.state()["classify"]["denials"]]

    def seed_stamp(self, sha: str) -> None:
        self.state_dir.mkdir(parents=True, exist_ok=True)
        (self.state_dir / "installed-sha").write_text(sha + "\n")

    def run_to_landed(self) -> str:
        self.assertExit(self.sync("prepare"), 0)
        self.assertExit(self.sync("gate"), 0)
        self.assertExit(self.sync("land"), 0)
        return self.state()["land"]["merge_sha"]


class PrepareTests(ForkSyncCase):
    def test_zero_new_commits_exits_clean(self) -> None:
        proc = self.sync("prepare")
        self.assertExit(proc, 0)
        self.assertIn("up to date", proc.stdout)
        self.assertEqual(self.state()["phase"], "up_to_date")
        self.assertFalse(self.worktree.exists())
        self.assertEqual(git(self.main, "rev-parse", "fork"), self.f0)

    def test_clean_merge_prepares_and_exits_0(self) -> None:
        u = self.upstream("docs tweak", {"docs/notes.md": "notes v2\n"})
        proc = self.sync("prepare")
        self.assertExit(proc, 0)
        state = self.state()
        self.assertEqual(state["phase"], "merged")
        self.assertEqual(state["upstream_sha"], u)
        self.assertEqual(state["fork_sha"], self.f0)
        self.assertEqual(state["commit_count"], 1)
        self.assertEqual(git(self.worktree, "rev-parse", "HEAD"), self.f0)
        self.assertEqual(git(self.worktree, "rev-parse", "MERGE_HEAD"), u)
        self.assertTrue(Path(state["report"]).name.endswith(f"-{git(self.main, 'rev-parse', '--short', u)}.md"))
        # the main checkout is untouched by prepare
        self.assertEqual(git(self.main, "rev-parse", "fork"), self.f0)
        self.assertEqual(git(self.main, "status", "--porcelain", "--untracked-files=no"), "")

    def test_textual_conflict_in_additive_file_exits_10(self) -> None:
        self.upstream("upstream field", {"src/config/model.rs": "fn base_one() {}\nfn base_two() {}\nfn upstream_added() {}\n"})
        proc = self.sync("prepare")
        self.assertExit(proc, 10)
        self.assertIn("src/config/model.rs", proc.stdout)
        conflicts = self.state()["classify"]["conflicts"]
        self.assertEqual([c["path"] for c in conflicts], ["src/config/model.rs"])
        self.assertEqual(conflicts[0]["rule"], "additive")
        self.assertIn("|||||||", (self.worktree / "src/config/model.rs").read_text())  # zdiff3 base marker
        self.assertIn("src/config/model.rs", self.report())

    def test_textual_conflict_in_deny_file_exits_20(self) -> None:
        self.upstream("upstream edit", {"src/shared.rs": SHARED.replace("shared line 5\n", "shared line 5 (up)\n")})
        proc = self.sync("prepare")
        self.assertExit(proc, 20)
        self.assertIn("deny-on-conflict file", self.denial_kinds())
        self.assertIn("<<<<<<<", self.report())
        probe = subprocess.run(["git", "-C", str(self.worktree), "rev-parse", "-q", "--verify", "MERGE_HEAD"], capture_output=True)
        self.assertNotEqual(probe.returncode, 0, "a denied prepare never starts the merge")

    def test_upstream_rename_of_fork_file_denied_even_when_clean(self) -> None:
        u = self.upstream("rename", rename=[("src/shared.rs", "src/shared2.rs")])
        git(self.main, "fetch", "-q", "origin")
        clean = subprocess.run(
            ["git", "-C", str(self.main), "merge-tree", "--write-tree", self.f0, u], capture_output=True
        )
        self.assertEqual(clean.returncode, 0, "git itself merges the rename cleanly")
        proc = self.sync("prepare")
        self.assertExit(proc, 20)
        self.assertIn("upstream renamed a fork path", self.denial_kinds())

    def test_upstream_modify_delete_denied(self) -> None:
        self.upstream("drop shared", delete=["src/shared.rs"])
        proc = self.sync("prepare")
        self.assertExit(proc, 20)
        kinds = self.denial_kinds()
        self.assertIn("upstream deleted a fork path", kinds)
        self.assertIn("non-content conflict", kinds)
        self.assertNotIn("manifest drift", kinds)

    def test_watch_list_identifier_in_incoming_diff_denied(self) -> None:
        self.upstream("collision", {"src/server/core.rs": "fn core() {}\nstruct S { transcript_path: PathBuf }\n"})
        proc = self.sync("prepare")
        self.assertExit(proc, 20)
        hits = self.state()["classify"]["watch_hits"]
        self.assertEqual([h["id"] for h in hits], ["transcript_path:"])
        self.assertIn("upstream collision", self.denial_kinds())

    def test_watch_list_ignores_removed_lines_vendor_and_case(self) -> None:
        write(self.up, {"src/server/old.rs": "enum E { Suspended }\n", "vendor/ghostty/x.zig": "a\n"})
        self.commit(self.up, "seed")
        git(self.main, "pull", "-q", "--no-rebase", "--no-edit", "origin", "master")
        self.f0 = git(self.main, "rev-parse", "fork")
        git(self.main, "push", "-q", "mine", "fork")
        self.upstream(
            "harmless",
            {"vendor/ghostty/x.zig": "const Suspended = 1;\n", "src/server/core.rs": "fn core() { suspended() }\n"},
            delete=["src/server/old.rs"],
        )
        proc = self.sync("prepare")
        self.assertExit(proc, 0)
        self.assertEqual(self.state()["classify"]["watch_hits"], [])

    def test_protocol_change_is_warned_and_flagged_for_review(self) -> None:
        self.upstream("bump", {"src/protocol/wire.rs": "pub const PROTOCOL_VERSION: u32 = 23;\n"})
        self.assertExit(self.sync("prepare"), 0)
        classify = self.state()["classify"]
        self.assertIn("PROTOCOL_VERSION 22 -> 23", classify["warnings"])
        self.assertEqual(classify["review_required"], ["src/protocol/wire.rs"])
        self.assertIn("deserialize_client_shell_agent_status", self.report())

    def test_section_1_drift_on_merge_result_denied(self) -> None:
        write(self.main, {"src/app/unlisted_fork_file.rs": "fn x() {}\n"})
        self.f0 = self.commit(self.main, "fork adds an unlisted file")
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 20)
        self.assertIn("manifest drift", self.denial_kinds())
        self.assertEqual(self.state()["classify"]["drift"]["unlisted"], ["src/app/unlisted_fork_file.rs"])

    def test_upstream_adding_fork_owned_path_denied(self) -> None:
        self.upstream("collision", {"src/app/agent_suspend.rs": "fn upstream_suspend() {}\n"})
        self.assertExit(self.sync("prepare"), 20)
        self.assertIn("upstream collision", self.denial_kinds())

    def test_missing_fork_md_denied(self) -> None:
        git(self.main, "rm", "-q", "FORK.md")
        self.f0 = self.commit(self.main, "drop manifest")
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 20)
        self.assertIn("no manifest", self.denial_kinds())

    def test_rerun_after_crash_mid_merge_recovers(self) -> None:
        u = self.upstream("upstream field", {"src/config/model.rs": "fn base_one() {}\nfn base_two() {}\nfn upstream_added() {}\n"})
        self.assertExit(self.sync("prepare"), 10)
        # crash: half-edited file, MERGE_HEAD left behind, state stuck in "merging"
        (self.worktree / "src/config/model.rs").write_text("garbage\n")
        (self.worktree / "stray.txt").write_text("left over\n")
        subprocess.run(
            [sys.executable, str(SCRIPTS / "fork_sync_lib.py"), "set",
             str(self.main / ".local/fork-sync/state.json"), "phase=merging"],
            check=True,
        )
        proc = self.sync("prepare")
        self.assertExit(proc, 10)
        self.assertIn("discarding in-progress run", proc.stdout)
        self.assertEqual(git(self.worktree, "rev-parse", "MERGE_HEAD"), u)
        self.assertIn("<<<<<<<", (self.worktree / "src/config/model.rs").read_text())
        self.assertFalse((self.worktree / "stray.txt").exists())
        worktrees = git(self.main, "worktree", "list", "--porcelain").count("worktree ")
        self.assertEqual(worktrees, 2)

    def test_rerun_after_worktree_deleted_recovers(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        subprocess.run(["rm", "-rf", str(self.worktree)], check=True)
        self.assertExit(self.sync("prepare"), 0)
        self.assertEqual(self.state()["phase"], "merged")

    def test_lock_busy_exits_75(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        lock = git(self.main, "rev-parse", "--path-format=absolute", "--git-common-dir") + "/fork-sync.lock"
        holder = subprocess.Popen(["lockf", "-k", "-t", "0", lock, "sleep", "30"])
        try:
            for _ in range(50):
                probe = subprocess.run(["lockf", "-k", "-t", "0", lock, "true"], capture_output=True)
                if probe.returncode != 0:
                    break
            proc = self.sync("prepare")
            self.assertExit(proc, 75)
            self.assertIn("lock             held", self.sync("status").stdout)
        finally:
            holder.kill()
            holder.wait()


class GateTests(ForkSyncCase):
    def _conflict(self) -> None:
        self.upstream("upstream field", {"src/config/model.rs": "fn base_one() {}\nfn base_two() {}\nfn upstream_added() {}\n"})
        self.assertExit(self.sync("prepare"), 10)

    def test_gate_refuses_unresolved_conflicts(self) -> None:
        self._conflict()
        proc = self.sync("gate")
        self.assertExit(proc, 1)
        self.assertIn("unresolved conflicts", proc.stderr)

    def test_gate_refuses_leftover_markers(self) -> None:
        self._conflict()
        git(self.worktree, "add", "src/config/model.rs")
        proc = self.sync("gate")
        self.assertExit(proc, 1)
        self.assertIn("conflict markers", proc.stderr)

    def test_resolved_additive_conflict_gates_lands_and_installs(self) -> None:
        self._conflict()
        (self.worktree / "src/config/model.rs").write_text(
            "fn base_one() {}\nfn base_two() {}\nfn upstream_added() {}\nfn fork_added() {}\n"
        )
        self.assertExit(self.sync("note", "src/config/model.rs: rule 7 additive, upstream first, fork last"), 0)
        self.assertExit(self.sync("gate"), 0)
        self.assertEqual(self.state()["gate"]["result"], "passed")
        self.assertExit(self.sync("land"), 0)
        m = git(self.main, "rev-parse", "fork")
        self.assertEqual(git(self.main, "rev-parse", f"{m}^1"), self.f0)
        self.assertIn("rule 7 additive", self.report())
        self.seed_stamp(self.f0)
        self.assertExit(self.sync("install"), 0)

    def test_gate_e0063_on_section_2_field_exits_10(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        log = (
            "error[E0063]: missing field `transcript_path` in initializer of `PersistedAgentSession`\n"
            "  --> src/app/api.rs:12:5\n"
            "error[E0063]: missing fields `agent_transcript_backup_last` and `backup_agent_transcripts` "
            "in initializer of `App`\n"
            "error[E0027]: pattern does not mention field `transcript_path`\n"
        )
        log_file = self.root / "cargo.log"
        log_file.write_text(log)
        proc = self.sync("gate", FORK_SYNC_GATE_CMD=f"cat '{log_file}'; exit 101")
        self.assertExit(proc, 10)
        gate = self.state()["gate"]
        self.assertEqual(gate["result"], "e0063")
        self.assertEqual(gate["failed_step"], "stub")
        self.assertEqual(len(gate["analysis"]["missing"]), 4)
        self.assertEqual(sorted(gate["analysis"]["codes"]), ["E0027", "E0063"])
        self.assertEqual(gate["analysis"]["missing"][0]["at"], "src/app/api.rs:12:5")
        self.assertIn("missing `transcript_path` at src/app/api.rs:12:5", self.report())
        # after the agent inserts defaults the gate is rerun and passes
        self.assertExit(self.sync("gate"), 0)

    def test_gate_other_failure_denied(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        cases = [
            "error[E0063]: missing field `brand_new` in initializer of `Thing`",
            "error[E0004]: non-exhaustive patterns: `AgentStatus::Suspended` not covered",
            "test fork_smoke::suspend_status_reaches_client_snapshot ... FAILED",
        ]
        for text in cases:
            with self.subTest(text=text):
                proc = self.sync("gate", FORK_SYNC_GATE_CMD=f"echo '{text}'; exit 1")
                self.assertExit(proc, 20)
                self.assertEqual(self.state()["gate"]["result"], "failed")
        self.assertExit(self.sync("land"), 1)

    def test_gate_writing_unexpected_files_is_denied(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        proc = self.sync("gate", FORK_SYNC_GATE_CMD="echo junk > stray.txt")
        self.assertExit(proc, 20)
        self.assertIn("unexpected files", self.state()["gate"]["failed_step"])
        self.assertFalse((self.worktree / "stray.txt").exists(), "stray output is reverted")
        # a schema regeneration under docs/next/api is allowed
        proc = self.sync("gate", FORK_SYNC_GATE_CMD="mkdir -p docs/next/api && echo '{}' > docs/next/api/s.json")
        self.assertExit(proc, 0)

    def test_skip_gate_knob(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        self.assertExit(self.sync("gate", FORK_SYNC_SKIP_GATE="1"), 0)
        self.assertEqual(self.state()["gate"]["result"], "skipped")

    def test_gate_steps_list_names_every_check(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        proc = self.sync("gate-steps", FORK_SYNC_GATE_CMD="")
        self.assertExit(proc, 0)
        names = [line.split("\t", 1)[0] for line in proc.stdout.splitlines()]
        self.assertEqual(
            names,
            [
                "fmt", "clippy", "schema-regen", "schema-verify", "nextest", "maintenance-test",
                "ui-hot-path-architecture-test", "integration-assets-test", "config-reference-check",
                "docs-translation-parity",
                "smoke:server::headless::tests::fork_smoke::suspended_status_reaches_the_client_shell_snapshot",
                "release-build",
            ],
        )
        self.assertIn(
            "--no-tests=fail -E 'test(=server::headless::tests::fork_smoke::suspended_status_reaches_the_client_shell_snapshot)'",
            proc.stdout,
        )


class LandTests(ForkSyncCase):
    def _gated(self) -> str:
        u = self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        self.assertExit(self.sync("gate"), 0)
        return u

    def test_land_succeeds_fast_forward_and_push(self) -> None:
        u = self._gated()
        proc = self.sync("land")
        self.assertExit(proc, 0)
        m = git(self.main, "rev-parse", "fork")
        self.assertEqual(self.state()["land"]["merge_sha"], m)
        self.assertEqual(git(self.main, "rev-parse", f"{m}^1"), self.f0)
        self.assertEqual(git(self.main, "rev-parse", f"{m}^2"), u)
        short = git(self.main, "rev-parse", "--short", u)
        self.assertEqual(git(self.main, "log", "-1", "--format=%B", m), f"chore(sync): merge upstream/master {short} (1 commits)")
        self.assertEqual(git(self.mine, "rev-parse", "refs/heads/fork"), m)
        self.assertEqual((self.main / "docs/notes.md").read_text(), "v2\n")
        self.assertEqual(git(self.main, "status", "--porcelain", "--untracked-files=no"), "")
        self.assertEqual(git(self.main, "tag"), "")
        # idempotent rerun
        again = self.sync("land")
        self.assertExit(again, 0)
        self.assertIn("already landed", again.stdout)
        self.assertIn("already pushed", again.stdout)

    def test_land_refuses_when_fork_moved_and_parks(self) -> None:
        self._gated()
        write(self.main, {"docs/fork-only.md": "late fork commit\n"})
        moved = self.commit(self.main, "late fork commit")
        proc = self.sync("land")
        self.assertExit(proc, 30)
        self.assertIn("moved since prepare", proc.stdout)
        self.assertEqual(git(self.main, "rev-parse", "fork"), moved)
        state = self.state()
        parked = state["land"]["parked_ref"]
        self.assertTrue(parked.startswith("refs/fork-sync/"))
        self.assertEqual(git(self.main, "rev-parse", parked), state["land"]["merge_sha"])
        self.assertEqual(git(self.mine, "rev-parse", "refs/heads/fork"), self.f0)
        self.assertIn("prepare", state["land"]["command"])

    def test_land_refuses_on_dirty_checkout(self) -> None:
        self._gated()
        (self.main / "src/server/core.rs").write_text("fn core() { dirty }\n")
        proc = self.sync("land")
        self.assertExit(proc, 30)
        self.assertIn("uncommitted changes", proc.stdout)
        self.assertEqual(git(self.main, "rev-parse", "fork"), self.f0)
        m = self.state()["land"]["merge_sha"]
        self.assertIn(f"merge --ff-only {m}", self.state()["land"]["command"])
        # after the user cleans up, rerunning land finishes the job
        git(self.main, "checkout", "--", "src/server/core.rs")
        self.assertExit(self.sync("land"), 0)
        self.assertEqual(git(self.main, "rev-parse", "fork"), m)

    def test_land_refuses_when_main_not_on_fork(self) -> None:
        self._gated()
        git(self.main, "checkout", "-q", "master")
        proc = self.sync("land")
        self.assertExit(proc, 30)
        self.assertEqual(git(self.main, "rev-parse", "fork"), self.f0)

    def test_land_refuses_before_gate(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        self.assertExit(self.sync("land"), 1)

    def test_land_refuses_if_worktree_changed_after_gate(self) -> None:
        self._gated()
        (self.worktree / "docs/notes.md").write_text("edited after gate\n")
        proc = self.sync("land")
        self.assertExit(proc, 1)
        self.assertIn("changed after the gate", proc.stderr)

    def test_dry_run_does_not_land_push_or_install(self) -> None:
        self._gated()
        proc = self.sync("land", FORK_SYNC_DRY_RUN="1")
        self.assertExit(proc, 0)
        self.assertIn("dry run", proc.stdout)
        self.assertEqual(git(self.main, "rev-parse", "fork"), self.f0)
        self.assertEqual(git(self.mine, "rev-parse", "refs/heads/fork"), self.f0)
        self.assertExit(self.sync("install", FORK_SYNC_DRY_RUN="1"), 0)
        self.assertFalse(self.bin_dir.exists())

    def test_reprepare_parks_an_unlanded_merge(self) -> None:
        self._gated()
        self.assertExit(self.sync("land", FORK_SYNC_DRY_RUN="1"), 0)
        state = self.state()
        m, run_id = state["land"]["merge_sha"], state["run_id"]
        self.upstream("more", {"docs/notes.md": "v3\n"})
        time.sleep(1.1)  # run ids have one-second resolution
        proc = self.sync("prepare")
        self.assertExit(proc, 0)
        self.assertEqual(git(self.main, "rev-parse", f"refs/fork-sync/{run_id}"), m)

    def test_abort_drops_merge_and_keeps_report(self) -> None:
        self.upstream("upstream field", {"src/config/model.rs": "fn base_one() {}\nfn base_two() {}\nfn upstream_added() {}\n"})
        self.assertExit(self.sync("prepare"), 10)
        report = Path(self.state()["report"])
        self.assertExit(self.sync("abort"), 0)
        self.assertEqual(self.state()["phase"], "aborted")
        self.assertTrue(report.exists())
        self.assertEqual(git(self.worktree, "rev-parse", "HEAD"), self.f0)
        probe = subprocess.run(["git", "-C", str(self.worktree), "rev-parse", "-q", "--verify", "MERGE_HEAD"], capture_output=True)
        self.assertNotEqual(probe.returncode, 0)
        self.assertExit(self.sync("gate"), 1)

    def test_up_to_date_after_land_reports_pending_install(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.run_to_landed()
        proc = self.sync("prepare")
        self.assertExit(proc, 0)
        self.assertIn("pending: install", proc.stdout)
        self.assertNotIn("pending: push", proc.stdout)


class InstallTests(ForkSyncCase):
    def test_protocol_change_stages_herdr_next(self) -> None:
        self.upstream("bump", {"src/protocol/wire.rs": "pub const PROTOCOL_VERSION: u32 = 23;\n"})
        self.seed_stamp(self.f0)
        self.bin_dir.mkdir()
        (self.bin_dir / "herdr").write_text("old build\n")
        m = self.run_to_landed()
        proc = self.sync("install")
        self.assertExit(proc, 40)
        self.assertIn("install + server restart together (protocol 22→23)", proc.stdout)
        self.assertEqual((self.bin_dir / "herdr").read_text(), "old build\n")
        self.assertEqual((self.bin_dir / "herdr.next").read_text(), self.built.read_text())
        self.assertEqual((self.state_dir / "installed-sha").read_text().strip(), self.f0)
        self.assertEqual(self.state()["install"]["result"], "staged")
        self.assertIn(m, self.state()["install"]["command"])

    def test_no_stamp_stages_instead_of_installing(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.run_to_landed()
        self.assertExit(self.sync("install"), 40)
        self.assertIn("no installed-sha stamp", self.state()["install"]["reason"])
        self.assertFalse((self.bin_dir / "herdr").exists())

    def test_unchanged_protocol_installs_atomically_with_server_restart_verdict(self) -> None:
        self.upstream("server change", {"src/server/core.rs": "fn core() { v2 }\n"})
        self.seed_stamp(self.f0)
        self.bin_dir.mkdir()
        (self.bin_dir / "herdr").write_text("old build\n")
        m = self.run_to_landed()
        proc = self.sync("install")
        self.assertExit(proc, 0)
        self.assertIn("server restart needed for: src/server/core.rs", proc.stdout)
        self.assertEqual((self.bin_dir / "herdr").read_text(), self.built.read_text())
        self.assertEqual((self.bin_dir / "herdr.prev").read_text(), "old build\n")
        self.assertFalse((self.bin_dir / "herdr.new").exists())
        self.assertEqual((self.state_dir / "installed-sha").read_text().strip(), m)
        again = self.sync("install")
        self.assertExit(again, 0)
        self.assertIn("already installed", again.stdout)

    def test_client_only_change_is_reattach(self) -> None:
        self.upstream("client change", {"src/client/view.rs": "fn view() { v2 }\n", "docs/notes.md": "v2\n"})
        self.seed_stamp(self.f0)
        self.run_to_landed()
        proc = self.sync("install")
        self.assertExit(proc, 0)
        self.assertIn("reattach needed", proc.stdout)
        self.assertEqual(self.state()["install"]["verdict"], "reattach needed")

    def test_non_src_build_input_change_is_server_restart(self) -> None:
        self.upstream("deps", {"Cargo.lock": "# lock v2\n", "docs/notes.md": "v2\n"})
        self.seed_stamp(self.f0)
        self.run_to_landed()
        proc = self.sync("install")
        self.assertExit(proc, 0)
        self.assertIn("server restart needed for: Cargo.lock", proc.stdout)

    def test_install_requires_landing(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        self.assertExit(self.sync("gate"), 0)
        self.assertExit(self.sync("install"), 1)


class StatusTests(ForkSyncCase):
    def test_status_reports_refs_lock_and_phase(self) -> None:
        self.upstream("docs", {"docs/notes.md": "v2\n"})
        self.assertExit(self.sync("prepare"), 0)
        proc = self.sync("status")
        self.assertExit(proc, 0)
        self.assertIn(f"fork             {self.f0}", proc.stdout)
        self.assertIn("lock             free", proc.stdout)
        self.assertIn("phase=merged", proc.stdout)
        self.assertIn("incoming         1 commit(s)", proc.stdout)


REAL_FORK_MD = Path(os.environ.get("FORK_SYNC_TEST_FORK_MD", SCRIPTS.parent / "FORK.md"))


class ForkMdParserTests(unittest.TestCase):
    def test_parses_fixture_sections(self) -> None:
        manifest = fork_sync_lib.parse_fork_md(FORK_MD)
        self.assertEqual(manifest["owned"], ["FORK.md", "src/app/agent_suspend.rs"])
        self.assertEqual(
            [f["field"] for f in manifest["fields"]],
            ["transcript_path", "backup_agent_transcripts", "agent_transcript_backup_last"],
        )
        rules = {r["path"]: r["rule"] for r in manifest["rules"]}
        self.assertEqual(
            rules,
            {
                "docs/next/CHANGELOG.md": "take_theirs",
                "skills/herdr/SKILL.md": "take_theirs_reapply",
                "src/config/model.rs": "additive",
                "src/server/client_commands.rs": "client_commands",
                "src/api/schema/common.rs": "deny",
            },
        )
        self.assertEqual(manifest["default_rule"], "deny")
        self.assertEqual(manifest["surfaces"], ["src/protocol/wire.rs", "src/client/shell/input.rs"])
        self.assertEqual(manifest["watch"], ["agent.suspend", "Suspended", '"suspended"', "transcript_path:"])
        self.assertEqual(
            manifest["smoke_tests"], ["server::headless::tests::fork_smoke::suspended_status_reaches_the_client_shell_snapshot"]
        )

    @unittest.skipUnless(REAL_FORK_MD.is_file(), "FORK.md not present in this checkout")
    def test_parses_the_real_fork_md(self) -> None:
        manifest = fork_sync_lib.parse_fork_md(REAL_FORK_MD.read_text())
        self.assertEqual(manifest["sections"], list(range(1, 13)))
        self.assertIn("FORK.md", manifest["owned"])
        self.assertTrue(all(" " not in p for p in manifest["owned"]))
        self.assertTrue(manifest["fields"] and all(f["field"] and f["default"] for f in manifest["fields"]))
        self.assertNotIn("field", [f["field"] for f in manifest["fields"]])
        self.assertEqual(manifest["default_rule"], "deny")
        self.assertTrue(all(r["path"] != "*" and " " not in r["path"] for r in manifest["rules"]))
        self.assertIn(("src/server/client_commands.rs", "client_commands"), [(r["path"], r["rule"]) for r in manifest["rules"]])
        self.assertTrue(all(" " not in p for p in manifest["surfaces"]))
        self.assertTrue(manifest["watch"] and all(w == w.strip() for w in manifest["watch"]))
        self.assertTrue(manifest["smoke_tests"] and all("fork_smoke" in t for t in manifest["smoke_tests"]))

    def test_draft_format_is_still_understood(self) -> None:
        text = (
            "# FORK.md\n\n## 1. Owned files\n```\n- `src/a.rs`\n- src/b.rs\n```\n\n"
            "## 7. Per-file merge rules\n```\nsrc/config/model.rs, keybinds.rs   additive, fork last\n"
            "Cargo.lock, Cargo.toml            take theirs; --locked gate verifies\n"
            "Everything else                   deny on conflict\n```\n"
        )
        manifest = fork_sync_lib.parse_fork_md(text)
        self.assertEqual(manifest["owned"], ["src/a.rs", "src/b.rs"])
        rules = {r["path"]: r["rule"] for r in manifest["rules"]}
        self.assertEqual(
            rules,
            {
                "src/config/model.rs": "additive",
                "src/config/keybinds.rs": "additive",
                "Cargo.lock": "take_theirs",
                "Cargo.toml": "take_theirs",
            },
        )

    def test_gate_log_analysis(self) -> None:
        manifest = fork_sync_lib.parse_fork_md(FORK_MD)
        ok = fork_sync_lib.analyze_gate_log(
            "error[E0027]: pattern does not mention fields `transcript_path`, `agent_transcript_backup_last`\n", manifest
        )
        self.assertTrue(ok["e0063_only"])
        mixed = fork_sync_lib.analyze_gate_log(
            "error[E0063]: missing field `transcript_path` in initializer of `X`\nerror[E0425]: cannot find function\n",
            manifest,
        )
        self.assertFalse(mixed["e0063_only"])


if __name__ == "__main__":
    unittest.main()
