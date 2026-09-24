"""Read-only helpers for scripts/fork_sync.sh.

The shell script owns every mutating step (fetch, worktree, merge, commit,
land, push, install). This module only reads git objects, parses FORK.md,
classifies an incoming upstream range, keeps state.json and renders the
markdown report. Nothing here writes to a git repository.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

EXIT_CLEAN = 0
EXIT_CONFLICTS = 10
EXIT_DENIED = 20

MAX_CONFLICT_HUNKS = 8

PROTOCOL_FILES = {
    "protocol_version": ("src/protocol/wire.rs", "PROTOCOL_VERSION"),
    "endpoint_generation": ("src/protocol/endpoint.rs", "ENDPOINT_PROTOCOL_GENERATION"),
}

# Rules an agent may apply to a conflicted file (FORK.md section 7 keywords).
RULE_KEYWORDS = {
    "take-theirs": "take_theirs",
    "take-theirs+reapply": "take_theirs_reapply",
    "additive": "additive",
    "section-5": "client_commands",
    "deny": "deny",
}
ALLOWED_RULES = ("take_theirs", "take_theirs_reapply", "additive", "client_commands")

# Compile errors section 2 turns into a mechanical fix (E0063 missing field in
# a struct literal, E0027 exhaustive struct pattern missing a field).
FIELD_ERROR_CODES = {"E0063", "E0027"}

# Pathspec for the drift check and the watch-list diff (FORK.md header).
DRIFT_PATHSPEC = (".", ":(exclude)docs/next/api")
WATCH_PATHSPEC = (".", ":(exclude)vendor")


# --------------------------------------------------------------------------
# git plumbing (read-only)


def git(repo: str, *args: str, check: bool = True) -> str:
    proc = subprocess.run(
        ["git", "-C", repo, *args],
        check=False,
        capture_output=True,
        text=True,
    )
    if check and proc.returncode != 0:
        raise SystemExit(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout


def git_show(repo: str, rev: str, path: str) -> str | None:
    proc = subprocess.run(
        ["git", "-C", repo, "show", f"{rev}:{path}"],
        check=False,
        capture_output=True,
        text=True,
    )
    return proc.stdout if proc.returncode == 0 else None


# --------------------------------------------------------------------------
# FORK.md


SECTION_RE = re.compile(r"^##\s+(\d+)\.\s*(.*)$")


def split_sections(text: str) -> dict[int, list[str]]:
    sections: dict[int, list[str]] = {}
    current: int | None = None
    for raw in text.splitlines():
        match = SECTION_RE.match(raw.strip())
        if match:
            current = int(match.group(1))
            sections[current] = []
            continue
        if raw.startswith("# ") or (raw.startswith("## ") and not match):
            current = None
            continue
        if current is None:
            continue
        line = raw.rstrip()
        stripped = line.strip()
        if not stripped or stripped.startswith("```") or stripped.startswith("<!--"):
            continue
        if stripped.startswith(">"):
            continue
        sections[current].append(line)
    return sections


def _clean_item(item: str) -> str:
    item = item.strip()
    if item.startswith("- ") or item.startswith("* "):
        item = item[2:].strip()
    return item.strip("`").strip()


def expand_paths(spec: str) -> list[str]:
    """`src/config/model.rs, keybinds.rs` -> both paths under src/config/."""
    out: list[str] = []
    current_dir = ""
    for part in re.split(r",\s*|\s+", spec):
        part = _clean_item(part)
        if not part:
            continue
        if "/" in part:
            current_dir = part.rsplit("/", 1)[0] + "/"
            out.append(part)
        else:
            out.append(current_dir + part)
    return out


def classify_rule(rule: str) -> str:
    keyword = rule.split(":", 1)[0].strip().lower()
    if keyword in RULE_KEYWORDS:
        return RULE_KEYWORDS[keyword]
    lowered = rule.lower()
    if "deny" in lowered:
        return "deny"
    if "take theirs" in lowered:
        return "take_theirs"
    if "section 5" in lowered:
        return "client_commands"
    if "additive" in lowered:
        return "additive"
    return "deny"


def parse_fork_md(text: str) -> dict[str, Any]:
    sections = split_sections(text)
    owned = []
    for line in sections.get(1, []):
        item = _clean_item(line)
        if item:
            owned.append(item.split()[0])

    fields = []
    for line in sections.get(2, []):
        stripped = line.strip()
        if not stripped.startswith("|"):
            continue
        cells = [c.strip().strip("`") for c in stripped.strip("|").split("|")]
        if len(cells) < 2 or cells[0].lower() == "struct" or set(cells[0]) <= set("-: "):
            continue
        for field in cells[1].split(","):
            field = field.strip().strip("`")
            if field:
                fields.append(
                    {"struct": cells[0], "field": field, "default": cells[2] if len(cells) > 2 else ""}
                )

    rules = []
    default_rule = "deny"
    for line in sections.get(7, []):
        parts = re.split(r"\s{2,}|\t", line.strip(), maxsplit=1)
        if len(parts) < 2:
            continue
        spec, rule_text = parts[0].strip(), parts[1].strip()
        kind = classify_rule(rule_text)
        if spec == "*" or spec.lower().startswith("everything else"):
            default_rule = kind
            continue
        for path in expand_paths(spec):
            rules.append({"path": path, "rule": kind, "text": rule_text})

    # `<path>  mid-logic: ...` / `<path>  depends: ...`
    surfaces = []
    surface_notes = {}
    for line in sections.get(8, []):
        parts = re.split(r"\s{2,}|\t", line.strip(), maxsplit=1)
        for path in expand_paths(parts[0]):
            surfaces.append(path)
            if len(parts) > 1:
                surface_notes[path] = parts[1].strip()

    # One fixed-string, case-sensitive identifier per line.
    watch = []
    for line in sections.get(9, []):
        token = line.strip()
        if token and token not in watch:
            watch.append(token)

    smoke = []
    for line in sections.get(10, []):
        item = _clean_item(line)
        if item:
            smoke.append(item.split()[0])

    return {
        "sections": sorted(sections),
        "owned": owned,
        "fields": fields,
        "rules": rules,
        "default_rule": default_rule,
        "surfaces": surfaces,
        "surface_notes": surface_notes,
        "watch": watch,
        "smoke_tests": smoke,
    }


def rule_for(manifest: dict[str, Any], path: str) -> dict[str, str]:
    for entry in manifest["rules"]:
        if entry["path"] == path or fnmatch.fnmatch(path, entry["path"]):
            return entry
    return {"path": path, "rule": manifest["default_rule"], "text": "everything else"}


def matches_any(path: str, patterns: list[str]) -> bool:
    return any(path == p or fnmatch.fnmatch(path, p) for p in patterns)


# --------------------------------------------------------------------------
# classification


def read_constant(text: str | None, name: str) -> int | None:
    if text is None:
        return None
    match = re.search(rf"pub const {name}\s*:\s*u32\s*=\s*(\d+)", text)
    return int(match.group(1)) if match else None


def protocol_at(repo: str, rev: str) -> dict[str, int | None]:
    return {
        key: read_constant(git_show(repo, rev, path), const)
        for key, (path, const) in PROTOCOL_FILES.items()
    }


def cargo_version_at(repo: str, rev: str) -> str | None:
    text = git_show(repo, rev, "Cargo.toml")
    if text is None:
        return None
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        match = re.match(r'version\s*=\s*"([^"]+)"', stripped)
        if in_package and match:
            return match.group(1)
    return None


def merge_tree(repo: str, fork: str, upstream: str) -> dict[str, Any]:
    proc = subprocess.run(
        [
            "git", "-C", repo, "-c", "merge.conflictStyle=zdiff3",
            "merge-tree", "--write-tree", "--name-only", fork, upstream,
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode not in (0, 1):
        raise SystemExit(f"git merge-tree failed: {proc.stderr.strip()}")
    head, _, messages = proc.stdout.partition("\n\n")
    lines = head.splitlines()
    tree = lines[0].strip() if lines else ""
    files = sorted(set(line.strip() for line in lines[1:] if line.strip()))
    kinds: dict[str, set[str]] = {}
    for message in messages.splitlines():
        match = re.match(r"CONFLICT \(([^)]+)\):.*", message)
        if not match:
            continue
        for path in files:
            if path in message:
                kinds.setdefault(path, set()).add(match.group(1))
    return {
        "clean": proc.returncode == 0,
        "tree": tree,
        "files": files,
        "kinds": {k: sorted(v) for k, v in kinds.items()},
        "messages": [m for m in messages.splitlines() if m.strip()],
    }


def conflict_hunks(repo: str, tree: str, path: str) -> list[str]:
    text = git_show(repo, tree, path)
    if text is None:
        return []
    hunks: list[str] = []
    block: list[str] | None = None
    for line in text.splitlines():
        if line.startswith("<<<<<<< "):
            block = [line]
        elif block is not None:
            block.append(line)
            if line.startswith(">>>>>>> "):
                hunks.append("\n".join(block))
                block = None
    return hunks


def name_status(repo: str, base: str, rev: str) -> list[dict[str, str]]:
    out = git(repo, "diff", "--name-status", "-M", "-z", base, rev)
    parts = out.split("\0")
    entries = []
    i = 0
    while i < len(parts) and parts[i]:
        status = parts[i]
        if status[0] in "RC":
            entries.append({"status": status[0], "score": status, "old": parts[i + 1], "path": parts[i + 2]})
            i += 3
        else:
            entries.append({"status": status[0], "score": status, "old": parts[i + 1], "path": parts[i + 1]})
            i += 2
    return entries


def watch_hits(repo: str, base: str, upstream: str, watch: list[str]) -> list[dict[str, str]]:
    if not watch:
        return []
    diff = git(repo, "diff", "--no-color", "--no-ext-diff", "-U0", base, upstream, "--", *WATCH_PATHSPEC)
    hits = []
    current = ""
    for line in diff.splitlines():
        if line.startswith("diff --git "):
            match = re.match(r"diff --git a/(.*) b/(.*)$", line)
            current = match.group(2) if match else ""
            continue
        if line.startswith("+++ ") or line.startswith("--- "):
            continue
        if not line.startswith("+"):
            continue
        for ident in watch:
            if ident in line[1:]:
                hits.append({"id": ident, "file": current, "line": line[:160]})
    return hits


def classify(repo: str, fork: str, upstream: str, base: str, manifest: dict[str, Any]) -> dict[str, Any]:
    denials: list[dict[str, str]] = []
    warnings: list[str] = []

    mt = merge_tree(repo, fork, upstream)
    conflicts = []
    hunks: dict[str, list[str]] = {}
    for path in mt["files"]:
        kinds = mt["kinds"].get(path, ["content"])
        rule = rule_for(manifest, path)
        entry = {"path": path, "kinds": kinds, "rule": rule["rule"], "rule_text": rule["text"]}
        conflicts.append(entry)
        hunks[path] = conflict_hunks(repo, mt["tree"], path)
        if kinds != ["content"]:
            denials.append({"kind": "non-content conflict", "path": path, "detail": ", ".join(kinds)})
        elif path in manifest["owned"]:
            denials.append({"kind": "conflict on fork-owned file", "path": path, "detail": "section 1"})
        elif rule["rule"] not in ALLOWED_RULES:
            denials.append({"kind": "deny-on-conflict file", "path": path, "detail": f"section 7: {rule['text']}"})
    hunk_count = sum(len(h) for h in hunks.values())
    if hunk_count > MAX_CONFLICT_HUNKS:
        warnings.append(f"{hunk_count} conflicted hunks (> {MAX_CONFLICT_HUNKS})")

    fork_changed = set(filter(None, git(repo, "diff", "--no-renames", "--name-only", base, fork).splitlines()))
    owned = set(manifest["owned"])
    rule_paths = [r["path"] for r in manifest["rules"]]
    touch_set = []
    review = []
    for entry in name_status(repo, base, upstream):
        old, new, status = entry["old"], entry["path"], entry["status"]
        fork_path = old in fork_changed or old in owned or new in owned
        listed = matches_any(old, rule_paths) or matches_any(new, rule_paths)
        surface = matches_any(old, manifest["surfaces"]) or matches_any(new, manifest["surfaces"])
        if fork_path or listed or surface:
            touch_set.append(
                {
                    "status": entry["score"],
                    "path": new,
                    "old": old,
                    "fork_changed": fork_path,
                    "surface": surface,
                    "note": manifest["surface_notes"].get(old) or manifest["surface_notes"].get(new, ""),
                }
            )
        if surface:
            review.append(new)
        if fork_path and status in "RDT":
            what = {"R": "renamed", "D": "deleted", "T": "type-changed"}[status]
            detail = f"{old} -> {new}" if status == "R" else old
            denials.append({"kind": f"upstream {what} a fork path", "path": old, "detail": detail})
        elif status == "A" and new in owned:
            denials.append({"kind": "upstream collision", "path": new, "detail": "upstream added a fork-owned path"})

    hits = watch_hits(repo, base, upstream, manifest["watch"])
    seen = set()
    for hit in hits:
        key = (hit["id"], hit["file"])
        if key in seen:
            continue
        seen.add(key)
        denials.append({"kind": "upstream collision", "path": hit["file"], "detail": f"watch-list identifier {hit['id']}"})

    proto_from = protocol_at(repo, fork)
    proto_to = protocol_at(repo, upstream)
    for key in PROTOCOL_FILES:
        if proto_from[key] != proto_to[key]:
            warnings.append(f"{PROTOCOL_FILES[key][1]} {proto_from[key]} -> {proto_to[key]}")
    version_from = cargo_version_at(repo, fork)
    version_to = cargo_version_at(repo, upstream)
    if version_from != version_to:
        warnings.append(f"Cargo.toml version {version_from} -> {version_to}")
    if review:
        warnings.append(f"upstream touched {len(review)} extended surface(s) (section 8): human review")

    # Section 1 drift, on the merge result: files the fork adds on top of U.
    added = set(
        filter(
            None,
            git(repo, "diff", "--diff-filter=A", "--name-only", upstream, mt["tree"], "--", *DRIFT_PATHSPEC).splitlines(),
        )
    )
    upstream_deleted = {e["old"] for e in name_status(repo, base, upstream) if e["status"] in "DR"}
    drift = {
        "unlisted": sorted(added - owned - upstream_deleted),
        "missing": sorted(owned - added),
    }
    if drift["unlisted"] or drift["missing"]:
        denials.append(
            {
                "kind": "manifest drift",
                "path": "FORK.md",
                "detail": "section 1 != fork-added files on the merge result: "
                f"unlisted {drift['unlisted']}, not added {drift['missing']}",
            }
        )

    if denials:
        verdict = EXIT_DENIED
    elif conflicts:
        verdict = EXIT_CONFLICTS
    else:
        verdict = EXIT_CLEAN
    return {
        "verdict": verdict,
        "merge_tree": {"clean": mt["clean"], "tree": mt["tree"], "messages": mt["messages"]},
        "conflicts": conflicts,
        "hunks": hunks,
        "hunk_count": hunk_count,
        "denials": denials,
        "warnings": warnings,
        "touch_set": touch_set,
        "review_required": review,
        "watch_hits": hits,
        "protocol": {"from": proto_from, "to": proto_to},
        "cargo_version": {"from": version_from, "to": version_to},
        "drift": drift,
    }


# --------------------------------------------------------------------------
# gate log analysis and restart verdict


E0063_RE = re.compile(r"error\[E0063\]: missing fields? (.+?) in initializer of `([^`]+)`")
E0027_RE = re.compile(r"error\[E0027\]: pattern does not mention fields? (.+?)$", re.MULTILINE)


def analyze_gate_log(log: str, manifest: dict[str, Any]) -> dict[str, Any]:
    codes = sorted(set(re.findall(r"error\[(E\d{4})\]", log)))
    missing = []
    lines = log.splitlines()
    for index, line in enumerate(lines):
        location = ""
        for follow in lines[index + 1 : index + 4]:
            if follow.strip().startswith("-->"):
                location = follow.strip()[3:].strip()
                break
        match = E0063_RE.search(line)
        if match:
            for name in re.findall(r"`([^`]+)`", match.group(1)):
                missing.append({"code": "E0063", "field": name, "struct": match.group(2), "at": location})
        match = E0027_RE.search(line)
        if match:
            for name in re.findall(r"`([^`]+)`", match.group(1)):
                missing.append({"code": "E0027", "field": name, "struct": "(pattern)", "at": location})
    patterns = [f["field"] for f in manifest["fields"]]
    unknown = [m for m in missing if not any(fnmatch.fnmatch(m["field"], p) for p in patterns)]
    e0063_only = bool(codes) and set(codes) <= FIELD_ERROR_CODES and bool(missing) and not unknown
    return {"codes": codes, "missing": missing, "unknown": unknown, "e0063_only": e0063_only}


REATTACH_SAFE_PREFIXES = ("src/client/", "docs/", "tests/", "scripts/", "skills/", ".github/", ".claude/")


def reattach_safe(path: str) -> bool:
    """True when a change cannot alter the server half of the binary.

    Conservative: only src/client/**, test code, docs, scripts and markdown.
    Everything else (rest of src/, vendor/, Cargo.*, build.rs, .cargo/) means
    a server restart.
    """
    if path.startswith(REATTACH_SAFE_PREFIXES):
        return True
    if path.endswith(".md"):
        return True
    return path.startswith("src/") and ("/tests/" in path or path.endswith("/tests.rs"))


def restart_verdict(repo: str, before: str | None, after: str, proto_before: dict | None) -> dict[str, Any]:
    proto_after = protocol_at(repo, after)
    if proto_before is None and before:
        proto_before = protocol_at(repo, before)
    if before is None:
        return {
            "protocol_changed": True,
            "protocol": {"from": None, "to": proto_after},
            "verdict": f"install + server restart together (protocol ?→{proto_after['protocol_version']})",
            "server_files": [],
        }
    changed = [p for p in git(repo, "diff", "--name-only", before, after).splitlines() if p]
    server_files = [p for p in changed if not reattach_safe(p)]
    protocol_changed = proto_before != proto_after
    if protocol_changed:
        if proto_before["protocol_version"] != proto_after["protocol_version"]:
            delta = f"protocol {proto_before['protocol_version']}→{proto_after['protocol_version']}"
        else:
            delta = (
                f"endpoint generation {proto_before['endpoint_generation']}→{proto_after['endpoint_generation']}"
            )
        verdict = f"install + server restart together ({delta})"
    elif server_files:
        verdict = "server restart needed for: " + ", ".join(server_files)
    else:
        verdict = "reattach needed"
    return {
        "protocol_changed": protocol_changed,
        "protocol": {"from": proto_before, "to": proto_after},
        "verdict": verdict,
        "server_files": server_files,
    }


# --------------------------------------------------------------------------
# state.json


def load_state(path: str) -> dict[str, Any]:
    try:
        return json.loads(Path(path).read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


def save_state(path: str, state: dict[str, Any]) -> None:
    """Write state.json atomically and re-render the run's report from it."""
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(target.parent), prefix=".state.", suffix=".json")
    with os.fdopen(fd, "w") as handle:
        json.dump(state, handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(tmp, target)
    write_report(state)


def write_report(state: dict[str, Any]) -> None:
    report = state.get("report")
    if report:
        Path(report).parent.mkdir(parents=True, exist_ok=True)
        Path(report).write_text(render_report(state) + "\n")


def get_path(state: dict[str, Any], dotted: str) -> Any:
    value: Any = state
    for part in dotted.split("."):
        if not isinstance(value, dict) or part not in value:
            return None
        value = value[part]
    return value


def set_path(state: dict[str, Any], dotted: str, value: Any) -> None:
    parts = dotted.split(".")
    cursor = state
    for part in parts[:-1]:
        cursor = cursor.setdefault(part, {})
    cursor[parts[-1]] = value


# --------------------------------------------------------------------------
# report


def _short(sha: str | None) -> str:
    return sha[:8] if sha else "-"


def render_report(state: dict[str, Any]) -> str:
    c = state.get("classify") or {}
    out: list[str] = []
    out.append(f"# fork-sync {state.get('run_id', '?')}: {state.get('outcome', state.get('phase', '?'))}")
    out.append("")
    out.append(f"- Phase: `{state.get('phase')}`")
    out.append(
        f"- Upstream: {state.get('upstream_ref')} {_short(state.get('upstream_sha'))} "
        f"({state.get('commit_count', 0)} commits, base {_short(state.get('base_sha'))})"
    )
    out.append(f"- Fork tip at prepare (F0): {_short(state.get('fork_sha'))}")
    proto = c.get("protocol") or {}
    if proto:
        pf, pt = proto.get("from", {}), proto.get("to", {})
        out.append(
            f"- PROTOCOL_VERSION {pf.get('protocol_version')} → {pt.get('protocol_version')}, "
            f"ENDPOINT_PROTOCOL_GENERATION {pf.get('endpoint_generation')} → {pt.get('endpoint_generation')}"
        )
    cv = c.get("cargo_version") or {}
    if cv:
        out.append(f"- Cargo.toml version {cv.get('from')} → {cv.get('to')}")
    if state.get("upstream_sha") and state.get("fork_sha"):
        out.append(f"- Reproduce: `git merge-tree --write-tree {state['fork_sha']} {state['upstream_sha']}`")
    out.append(f"- Sync worktree: `{state.get('worktree')}` (CARGO_TARGET_DIR `{state.get('target_dir')}`)")
    out.append("")

    def section(title: str, items: list[str]) -> None:
        out.append(f"## {title}")
        out.append("")
        out.extend(items if items else ["(none)"])
        out.append("")

    section("Warnings", [f"- {w}" for w in c.get("warnings", []) + state.get("warnings", [])])
    section("Denials", [f"- **{d['kind']}**: `{d['path']}` ({d['detail']})" for d in c.get("denials", [])])
    section(
        "Conflicts",
        [
            f"- `{x['path']}` [{', '.join(x['kinds'])}] rule: {x['rule']} ({x['rule_text']})"
            for x in c.get("conflicts", [])
        ],
    )
    section(
        "Touch-set (upstream changes on fork paths and FORK.md surfaces)",
        [
            f"- {t['status']} `{t['old']}`" + (f" → `{t['path']}`" if t["old"] != t["path"] else "")
            + (" [fork-changed]" if t["fork_changed"] else "")
            + (f" [section 8 review: {t.get('note') or 'surface'}]" if t["surface"] else "")
            for t in c.get("touch_set", [])
        ],
    )
    section("Watch-list hits", [f"- `{h['id']}` in `{h['file']}`: `{h['line']}`" for h in c.get("watch_hits", [])])
    drift = c.get("drift") or {}
    if drift.get("unlisted") or drift.get("missing"):
        section(
            "FORK.md section 1 drift",
            [f"- unlisted fork-added: `{p}`" for p in drift.get("unlisted", [])]
            + [f"- listed but absent: `{p}`" for p in drift.get("missing", [])],
        )
    denied_paths = {d["path"] for d in c.get("denials", [])}
    hunk_lines: list[str] = []
    for path, hunks in (c.get("hunks") or {}).items():
        label = "denied" if path in denied_paths else "to resolve"
        for hunk in hunks:
            hunk_lines.append(f"`{path}` ({label}):")
            hunk_lines.append("```")
            hunk_lines.append(hunk)
            hunk_lines.append("```")
    section("Conflict hunks (zdiff3)", hunk_lines)
    section("Agent notes", [f"- {n}" for n in state.get("notes", [])])

    gate = state.get("gate") or {}
    gate_lines = []
    if gate:
        gate_lines.append(f"- Result: **{gate.get('result')}**")
        for step in gate.get("steps", []):
            gate_lines.append(f"- {step['name']}: rc={step['rc']} ({step.get('log', '')})")
        if gate.get("failed_step"):
            gate_lines.append(f"- Failed step: `{gate['failed_step']}`")
        analysis = gate.get("analysis") or {}
        if analysis.get("codes"):
            gate_lines.append(f"- Compiler error codes: {', '.join(analysis['codes'])}")
        for m in analysis.get("missing", []):
            gate_lines.append(
                f"  - {m.get('code', 'E0063')} `{m['struct']}` missing `{m['field']}` at {m.get('at') or '?'}"
            )
        if gate.get("tail"):
            gate_lines.append("```")
            gate_lines.append(gate["tail"])
            gate_lines.append("```")
    section("Gate", gate_lines)

    land = state.get("land") or {}
    land_lines = []
    if land:
        land_lines.append(f"- Merge commit M: `{land.get('merge_sha')}`")
        land_lines.append(f"- Landed: {land.get('landed')}")
        if land.get("parked_ref"):
            land_lines.append(f"- Parked at `{land['parked_ref']}` ({land.get('reason')})")
        if land.get("command"):
            land_lines.append(f"- Landing command: `{land['command']}`")
        land_lines.append(f"- Pushed to mine: {land.get('pushed')}")
        if land.get("push_command"):
            land_lines.append(f"- Push command: `{land['push_command']}`")
        if land.get("unpushed_fork_commits"):
            land_lines.append(f"- Also published {land['unpushed_fork_commits']} fork commit(s) that were not on mine/fork")
    section("Landing", land_lines)

    inst = state.get("install") or {}
    inst_lines = []
    if inst:
        inst_lines.append(f"- Result: **{inst.get('result')}**")
        for key in ("reason", "path", "baseline_sha", "command"):
            if inst.get(key):
                inst_lines.append(f"- {key}: `{inst[key]}`")
        if inst.get("verdict"):
            inst_lines.append(f"- Verdict: **{inst['verdict']}**")
    section("Install", inst_lines)

    rollback = []
    if land.get("landed") and state.get("fork_sha"):
        rollback.append(f"- fork: `git -C {state.get('main')} reset --keep {state['fork_sha']}` (and push mine by hand)")
    if inst.get("result") == "installed":
        rollback.append(f"- binary: `mv -f {inst.get('bin_dir')}/herdr.prev {inst.get('bin_dir')}/herdr`")
    section("Rollback", rollback)
    return "\n".join(out)


# --------------------------------------------------------------------------
# CLI


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("forkmd")
    p.add_argument("file")
    p.add_argument("--key", help="print one list item per line instead of JSON")

    p = sub.add_parser("classify")
    for name in ("repo", "fork", "upstream", "base", "forkmd"):
        p.add_argument(f"--{name}", required=True)
    p.add_argument("--state", help="store the result under state.classify and print verdict + conflict paths")

    p = sub.add_parser("gate-analyze")
    p.add_argument("log")
    p.add_argument("forkmd")
    p.add_argument("--state", help="store under state.gate.analysis and print e0063_only")

    p = sub.add_parser("restart")
    p.add_argument("--repo", required=True)
    p.add_argument("--before", default="")
    p.add_argument("--after", required=True)
    p.add_argument("--shell", action="store_true", help="print S_protocol_changed / S_verdict assignments")

    p = sub.add_parser("protocol")
    p.add_argument("--repo", required=True)
    p.add_argument("rev")

    p = sub.add_parser("get")
    p.add_argument("state")
    p.add_argument("key")

    p = sub.add_parser("set")
    p.add_argument("state")
    p.add_argument("pairs", nargs="+", help="key=string or key:=json")

    p = sub.add_parser("setfile")
    p.add_argument("state")
    p.add_argument("key")
    p.add_argument("file", help="JSON file whose content becomes the value")

    p = sub.add_parser("jget", help="read a dotted key from a JSON file")
    p.add_argument("file")
    p.add_argument("key")

    p = sub.add_parser("vars", help="print S_<key>=<value> shell assignments")
    p.add_argument("state")
    p.add_argument("keys", nargs="+")

    p = sub.add_parser("append")
    p.add_argument("state")
    p.add_argument("key")
    p.add_argument("value")

    p = sub.add_parser("report")
    p.add_argument("state")

    args = parser.parse_args(argv)

    if args.cmd == "forkmd":
        manifest = parse_fork_md(Path(args.file).read_text())
        if args.key:
            for item in manifest.get(args.key, []):
                print(item if isinstance(item, str) else json.dumps(item))
        else:
            print(json.dumps(manifest, indent=2))
    elif args.cmd == "classify":
        manifest = parse_fork_md(Path(args.forkmd).read_text())
        result = classify(args.repo, args.fork, args.upstream, args.base, manifest)
        if args.state:
            state = load_state(args.state)
            state["classify"] = result
            save_state(args.state, state)
            print(result["verdict"])
            for conflict in result["conflicts"]:
                print(conflict["path"])
        else:
            print(json.dumps(result))
    elif args.cmd == "gate-analyze":
        manifest = parse_fork_md(Path(args.forkmd).read_text())
        log = Path(args.log).read_text(errors="replace")
        analysis = analyze_gate_log(log, manifest)
        if args.state:
            state = load_state(args.state)
            set_path(state, "gate.analysis", analysis)
            save_state(args.state, state)
            print("true" if analysis["e0063_only"] else "false")
        else:
            print(json.dumps(analysis))
    elif args.cmd == "restart":
        result = restart_verdict(args.repo, args.before or None, args.after, None)
        if args.shell:
            print(f"S_protocol_changed={'true' if result['protocol_changed'] else 'false'}")
            print(f"S_verdict={shlex.quote(result['verdict'])}")
        else:
            print(json.dumps(result))
    elif args.cmd == "protocol":
        print(json.dumps(protocol_at(args.repo, args.rev)))
    elif args.cmd == "get":
        value = get_path(load_state(args.state), args.key)
        if value is None:
            return 0
        print(value if isinstance(value, str) else json.dumps(value))
    elif args.cmd == "set":
        state = load_state(args.state)
        for pair in args.pairs:
            eq = pair.index("=")
            if eq > 0 and pair[eq - 1] == ":":
                set_path(state, pair[: eq - 1], json.loads(pair[eq + 1 :]))
            else:
                set_path(state, pair[:eq], pair[eq + 1 :])
        save_state(args.state, state)
    elif args.cmd == "setfile":
        state = load_state(args.state)
        set_path(state, args.key, json.loads(Path(args.file).read_text()))
        save_state(args.state, state)
    elif args.cmd == "jget":
        value = get_path(json.loads(Path(args.file).read_text()), args.key)
        if value is not None:
            print(value if isinstance(value, str) else json.dumps(value))
    elif args.cmd == "vars":
        state = load_state(args.state)
        for key in args.keys:
            value = get_path(state, key)
            if value is None:
                value = ""
            elif not isinstance(value, str):
                value = json.dumps(value)
            print(f"S_{key.replace('.', '_')}={shlex.quote(value)}")
    elif args.cmd == "append":
        state = load_state(args.state)
        current = get_path(state, args.key) or []
        current.append(args.value)
        set_path(state, args.key, current)
        save_state(args.state, state)
    elif args.cmd == "report":
        write_report(load_state(args.state))
    return 0


if __name__ == "__main__":
    sys.exit(main())
