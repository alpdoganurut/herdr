# Brief — <slug> drills

## Rig facts
Endpoints, env, data root, ports, user-facing strings, storage names, timing constants.

## Drill list (FULL list at T0)
Include restore / rollback / version skew / ship-gap / paid (prerequisites checked at T0).

| drill | needs (WP) | asserts | state |
|---|---|---|---|
| | | | NEEDS <WP> |

## Rules
- Assert outcomes (rows, storage, request log, server log), never timing or order — detect the order and branch on it.
- A run names its target sha and is void if the tip moved; test a sha in a detached temp worktree.
- Per-member data root; age/pid-safe cleanup.

## Reporting
Per-run summary file + drill × sha tracker row. No status pings; verdicts by message.

## Runbook
You own it; others send edits.
