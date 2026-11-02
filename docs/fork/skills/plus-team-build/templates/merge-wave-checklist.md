# Merge checklist (integrator) — per hand-off / batch

## Before merge
- [ ] Hand-off row in handoffs.md: WP ids, branch@sha, base TIP, rulings ≤ R-NNN (no affecting ruling missed), targeted exit codes, audit summary, foreign-file commits.
- [ ] Branch is linear on the current tip (`merge-base --is-ancestor`), and `git cherry` shows only this lane's commits. Linear on the announced TIP it rebased onto; if the head moved by disjoint commits, integrator rebases `--onto` and confirms `git range-diff` patch-identical.
- [ ] If this is an assembled batch (several lanes on a shared base), run a cross-model audit on the BATCH (2 Claude + a second model such as Codex if installed, else per SKILL.md §0: 2 Opus auditors + judge) with seam focus items. Fix HIGH/MED first. seams.md rows touched by this batch have producer AND consumer at the tip.
- [ ] No tests are running in the worktree you merge into, and no commit is included that wasn't handed off.

## Merge
- [ ] FF or rebase-merge, then resolve conflicts. Conflicts in shared files follow the ownership.md merge order.
- [ ] Full gates on a FROZEN batch sha, in the dedicated clean gate worktree (never the merge worktree), serialized per machine, through the deadline wrapper, exit codes from files, output validity checked. Record `TIP <sha>: gate exit <n>, ratchet <n>` in the tip log. Nothing merges until it returns.
- [ ] Skip ratchet re-verified; lower the max when stale skips flipped. Flipped skips re-verified non-hollow before the max drops.
- [ ] Ship gate on a `git archive` export (it must FAIL on a bad tree; proven at T0).

## After merge
- [ ] Announce ONE `TIP <sha>` per window (identical message to all: "TIP <sha>. See brief header."); update the brief header + tip log; name who must rebase.
- [ ] New rulings appended to rulings.md (tracked); ownership.md updated if ownership moved.
- [ ] Report the sha and gate results (one message to the owner/lead; update the notes page too if the owner asked for one). At a wave boundary: delete superseded refs and fixer worktrees whose commits landed; every remaining ref/worktree has a §Refs row; report ref + worktree counts.

## Pre-deploy (final)
- [ ] For every code-bearing ruling: commit named and `git merge-base --is-ancestor <commit> <final>` passes; FINAL announced once with this map.
- [ ] tested sha == FF sha, or `git diff <tested> <tip>` touches only the spec dir and the GO report says so; main tree linear, no untracked-path collision.
- [ ] One full-branch read (rename detection on) @ <ship sha>, by the inspectors.
- [ ] Commits after the last batch audit listed, each with a seam re-audit or VERIFIED@sha.
- [ ] Architect GO, no HIGH/MED in OPEN, ROUTED or FIXED-unverified; each DEFERRED cites a ruling; drills PASS on the final client, UI findings landed or ruled by the owner.
- [ ] Gate OPEN on export; runbook verify lines + drain-first rollback written. Every prod change outside the artifact is a named runbook step with a verify line.
- [ ] GO report lists every PROVISIONAL owner-only ruling and every dropped step.
