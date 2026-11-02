# Rulings — <slug>

Tracked on the integration branch, append-only; the ONLY copy (no snapshots or logs beside it). Where this file and the spec differ, this file wins; the spec header says "rulings.md overrides". Fold into the spec only at close-out. Each ruling is committed BEFORE it is messaged.
Mark **[OWNER]** on rulings in the owner-only list (SKILL §7). If the owner is away, add **[PROVISIONAL]** when made; every one is listed in the GO report.

| id | time | ruling | why (failure timeline, short) | consumers who must act (member · file · test) | behaviour change | supersedes |
|---|---|---|---|---|---|---|
| R-001 | | contracts frozen @ <sha> | | all | — | — |
| R-002 | | seats for this build: <list> | from the work | all | — | — |

## Open — to be ruled (each with a named owner)
| topic | owner | due |
|---|---|---|
| lifecycle: version skew / interrupted op / rollback / restore (e.g. browser: link loss / reload) | | before the first consumer lane merges |
