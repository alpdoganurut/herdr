# Ownership — <slug>

Tracked on the integration branch. Update it in the same minute as any ruling that moves ownership.

## Roles
| Who | Merges into the lane | FF lane → main | Bumps the version | Ships | Told at FF time | Approves user-visible behaviour/copy |
|---|---|---|---|---|---|---|
| | integrator | | | | main-tree owner | owner |

## Seats → members
One member may hold several non-lane seats; a lane member never reviews its own lane. The seat list itself is a T0 ruling.

| Seat | Member |
|---|---|
| | |

## Refs (every branch and worktree, registered when created)
Cap: one worktree per active member + one per running parallel fixer. Close-out fails while a row remains.

| Branch | Worktree | Owner | Purpose | Base | Close-out (merge → archive → remove → `branch -d`) | State |
|---|---|---|---|---|---|---|
| | | | | | | open / merged / removed |

## Open items on departure
| Item (follow-up / DEFERRED / finding / ref) | From member | To owner | Ruling |
|---|---|---|---|

## Files
| Path / glob | Owner | Notes |
|---|---|---|
| | | test fixtures and helpers count as files |

## Shared files — merge order
| File | Content owner | Secondary writers (per wave) | Rule |
|---|---|---|---|
| e.g. generated API types | A | B | the last lane to merge regenerates; the others rebase |
| e.g. app wiring / module registry | … | … | … |
| any signature change | changer | — | the changer edits callers mechanically; the file owner rebases |

## Acceptance → owner → test
Write one row per behaviour the spec promises. A row whose behaviour spans two members' files still gets ONE owner. Write the test on day one as a TAGGED pending skip naming its owner (counted by the ratchet); the owner flips it when the behaviour lands. Structural gate tests (every route wired, pending list empty) are the exception and are RED inside the ship gate only.

| # | Acceptance criterion | Owner | Test (subject-named) | State |
|---|---|---|---|---|
| A-01 | | | | pending(<owner>) / green |

## Layout (new files)
| Kind | Target folder | Naming |
|---|---|---|
| | | by subject: never a lane letter, WP id or audit round |

Reorg of existing files happens at freeze (move-only, before fan-out) or is DEFERRED in writing in the spec dir; never mid-build. Each doc (runbook, spec) has one owner; others send edits. Per-suite test deadlines:

| suite | measured | deadline | STALL_S |
|---|---|---|---|
