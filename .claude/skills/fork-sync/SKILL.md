---
name: fork-sync
description: Merge upstream herdr master into the `fork` branch through scripts/fork_sync.sh. Resolves only what FORK.md sections 2 and 4-7 allow and denies everything else. Run it only when the user asks for /fork-sync.
model: sonnet
disable-model-invocation: true
allowed-tools: Bash(scripts/fork_sync.sh:*), Bash(git:*), Bash(python3:*), Read, Grep, Edit
---

# /fork-sync

Use Sonnet for this skill (`model: sonnet` above). **When unsure, deny. Don't reason your way around a rule.**

`scripts/fork_sync.sh` does everything deterministic: fetch, classify, merge in the detached sync worktree, gate, land, push and install. Your only job is to resolve the conflicts FORK.md explicitly allows. Everything else is a denial.

## Hard rules

- Never restart the herdr server or client, and never run `herdr server ...` or any other `herdr` command. The script stages or installs the binary; the user does any restart.
- Never edit digest values, `tests/fixtures/endpoint-*-v1.json`, frozen fixtures, or anything under `src/protocol/**` tests. A failing digest or wire test is a denial.
- Edit files only inside the sync worktree printed by `prepare` (`../herdr-worktrees/fork-sync`). Never edit the main checkout. The only git commands you run yourself are the rule 7 ones, all inside the sync worktree: `git checkout --theirs -- <path>`, `git add -- <path>` (only right after that checkout) and `git apply --3way`. Never run `git commit`, `merge`, `push`, `reset` or `stash`; the script does all of that.
- After you start resolving, never rerun `prepare`. It resets the sync worktree and throws your resolutions away. From there, run `gate`.
- Read FORK.md from the sync worktree. Rule numbers below are FORK.md section numbers.

## Flow

1. `scripts/fork_sync.sh prepare`, then read the report path it prints.
   - exit 0 + "up to date": nothing new. If it prints `pending: push`/`pending: install`, run that subcommand. Otherwise report "up to date" and stop.
   - exit 0: clean merge. Go to step 3.
   - exit 10: conflicts. Go to step 2.
   - exit 20: denied. Report the denials from the report and stop. Don't run anything else.
   - exit 75: another sync is running. Report that and stop.
2. Resolve each file listed under "Conflicts" in the report. Every conflict line shows the file's FORK.md section 7 rule (`take_theirs`, `take_theirs_reapply`, `additive`, `client_commands`). Apply only that rule, read with the detail text after the keyword in FORK.md section 7:
   - **Rule 7 `take-theirs`**: `git -C <worktree> checkout --theirs -- <path>`. For `docs/next/api/herdr-api.schema.json` that is all you do, because the gate regenerates and verifies it.
   - **Rule 7 `take-theirs+reapply`** (`skills/herdr/SKILL.md`): take theirs and `git -C <worktree> add -- skills/herdr/SKILL.md`, then `git -C <worktree> diff <base> <fork tip> -- skills/herdr/SKILL.md | git -C <worktree> apply --3way`. The base and fork tip (F0) SHAs are at the top of the report. If a hunk doesn't apply, deny.
   - **Rule 7 `additive`** (structural additive rule): allowed only when the zdiff3 base section (between `|||||||` and `=======`) is empty and both sides are pure insertions. Keep upstream's lines first and the fork's lines last, at the placement the rule's detail names, and delete nothing. If the base is non-empty or either side changes or deletes a line, deny.
   - **Rule 4 enum variants** (inside an additive file): the fork variant goes where section 4 says. `AgentStatus::Suspended` and anything marked append-closed or deny-on-conflict is a denial. Never re-sort an enum.
   - **Rule 5 `section-5`** (`src/server/client_commands.rs`): `CLIENT_SHELL_METHODS` is the sorted union. Digest assert blocks are the union of the `actual.remove(..)` blocks, upstream first, and no method name may appear twice. If upstream now advertises `pane.move` or adds any section 9 identifier, deny. If any digest value changed, deny.
   - **Rule 6 config keys**: in `config-reference.json` (additive), use the placement section 6 gives. Then `python3 -m json.tool <file> >/dev/null` and `python3 scripts/config_reference_check.py` (both run in the worktree) must pass.
   - Anything else, including a `deny` or `*` rule, a non-content conflict, or a hunk that doesn't fit its rule: run `scripts/fork_sync.sh abort` and report a denial.
   - After each file, record the rule you applied: `scripts/fork_sync.sh note "<path>: rule <N> <what you did>"`.
3. `scripts/fork_sync.sh gate`. This is the full gate plus the release build, so it takes a while.
   - exit 0: go to step 4.
   - exit 10: the only errors were E0063 (missing field in a struct literal) or E0027 (exhaustive struct pattern missing a field), all on FORK.md section 2 fields. In **upstream-authored code only**, insert the section 2 default for each E0063 site the report lists, or `<field>: _` for each E0027 pattern. Record a `note` quoting "rule 2", then rerun `gate`. Any other compiler error is a denial (E0004 is rule 4; E0425, E0061 and E0308 are rule 3), and so is any failing test.
   - exit 20: denied. Run `abort`, report the failing step and log path, and stop.
4. `scripts/fork_sync.sh land`.
   - exit 0: landed and pushed.
   - exit 30: parked. Don't retry. Report the one landing command it printed.
   - exit 31: landed, but the push failed. Report the printed push command.
5. `scripts/fork_sync.sh install` (only after land exits 0).
   - exit 0: installed (or already installed).
   - exit 40: staged as `~/.local/bin/herdr.next`. It was not installed because the protocol or endpoint generation changed or no stamp exists. Report the printed command.

`scripts/fork_sync.sh status` shows the current state at any point. `state.json` and the reports are in `.local/fork-sync/` in the main checkout.

## Report to the user

Report in exactly this order, one line or a short list each:

1. **Outcome**: `merged`, `merged (landing pending)`, `denied` or `up to date`.
2. **Upstream range**: `base..upstream` short SHAs and the commit count.
3. **Resolved**: each file with the rule number you applied (from your notes), or "nothing".
4. **Gate**: passed, or the failing step and its log path.
5. **Landing**: done (`fork` = M, pushed to mine), or the one command to run.
6. **Install**: installed, or staged as `herdr.next` with the command.
7. Last, exactly one of these (the report's install verdict):
   - `reattach needed`
   - `server restart needed for: <files>`
   - `install + server restart together (protocol N→M)`

For a denial, stop after the report path and the denial reasons. List them as the report states them, with the FORK.md rule each one broke.
