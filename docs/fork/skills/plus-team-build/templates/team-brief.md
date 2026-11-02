# Team brief — <slug> build (<date>)

> **STATE: HOLD | PREP | GO (code) | FROZEN-FOR-DEPLOY** · **TIP <sha> (<date -u>)** · **last ruling R-NNN** · **FINAL: no | <sha>** — rewritten in place by the integrator. Re-read before you act. Tip log (append-only) at the bottom: sha, landed, who must rebase, gate exit.

Integrator: **<name>** (`to="<name>"`). Report to the integrator, not to lead. The only exception is an issue that touches main or prod.
Lead owns main + prod. Roles (FF, bump, ship, who is told at FF) are in ownership.md §Roles.

## Read first (absolute paths)
- Spec of record: `<specs>/<slug>/<slug>.md`
- Rulings (override the spec): `<specs>/<slug>/rulings.md`. The integrator commits a ruling BEFORE messaging it. Read it via `git diff <last-read> <integration> -- rulings.md` before every subagent launch, before every hand-off, after every subagent return.
- Ownership + acceptance matrix + merge order: `<specs>/<slug>/ownership.md`
- Findings file: `<scratch>/<slug>/findings.md`
- Seams: `<specs>/<slug>/seams.md`
- Hand-offs: `<scratch>/<slug>/handoffs.md`

## Members
| Member | WPs | Branch / worktree | Owns → ownership.md |
|---|---|---|---|
| <slug>-<member> | … | `<prefix><slug>-<member>` / `../<repo>-<slug>-<member>` | … |

Overlap rule: if you need a file another member owns, message them. If you must edit it to stay green, use a separate commit and flag it in the hand-off.

## Hard rules (all members, from minute one)
- Tests: every run through `<path>/run-with-deadline.sh <max_s> <log> <cmd…>`; deadlines per suite from ownership.md (≈ 2× measured). Exit codes from the file, never a pipe; "0 tests ran" = FAIL; stall check sized for buffered reporters (heartbeat ≤ 60 s); never pass file lists via unquoted shell vars. A second hang is a finding → tagged pending skip. Known-flake list: `<path>`, owned by the gate runner.
- Lanes run targeted tests only. <integrator|tests member> runs the full gates once per merge batch, with workers capped at <n>. Full gates run only on an announced TIP, serialized per machine.
- Branch from the integration tip only. Right before each hand-off, rebase onto the latest ANNOUNCED tip (never a sha nobody announced), rerun targeted tests and verify: `git merge-base --is-ancestor <lane> <branch>`; `git cherry <lane> <branch>` shows only your commits.
- Hand-off = append one row to `<scratch>/<slug>/handoffs.md` (WP ids, branch@sha, base TIP, rulings ≤ R-NNN, targeted exit codes, audit summary, foreign-file commits) + one ≤ 1-line pointer message. The integrator reads the file at each merge window.
- Read points: before every subagent launch, before every hand-off, after every subagent return. Stamp every subagent prompt and hand-off with `TIP <sha> · rulings ≤ R-NNN`.
- Subagents:
  - Opus;
  - self-contained prompts;
  - write first, commit each finished item, never report done with uncommitted edits;
  - one worktree per parallel fixer;
  - a scope change restates the full final list;
  - subagents report to you only;
  - prompts say "verify the premise against the current tip first";
  - never message a finished subagent: relaunch with done/next, never "continue".
- Messages: ≤ 4000 chars. Anything longer goes in a file; send the path.
- <repo commit rules from CLAUDE.md: signing, trailers, version bumps>; scoped `git add` only.
- No prod access. Kill only your own processes (PID / process group).
- Rigs: per-member data root outside any worktree, free ports only, prune only your own dirs.
- Paid calls (delete if none): budget <$>; ask the integrator first; each call records its cost via <hook from CLAUDE.md>.

## First move
1. `agents_whoami`, then read this brief plus the spec sections for your WPs.
2. Create your worktree.
3. Send the integrator one message: your lane plan (sub-tasks → subagents), contract needs, risks.
4. Do not write code until STATE = GO and you have received "contracts frozen @ <sha>". If opened before the freeze: do the written pre-freeze task (path:line map of your area, contract asks, test list); no worktree yet.

## Tip log (append-only)
| sha | landed | who must rebase | gate exit |
|---|---|---|---|
