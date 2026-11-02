---
name: plus:team-build
description: Run a multi-agent build as a herdr+ team — design with the owner, freeze contracts, fan out lane members on an integration branch, review continuously, gate, cut over, monitor. Use when the owner says "build this with a team", "spin up members for X", "run it overnight with agents", or a feature is too big for one session (several lanes, a shared contract, one cutover). Also use to set up the integrator role for an existing team. Not for a single-session feature or a review-only task.
argument-hint: "[feature / spec path]"
user-invocable: true
---

# Team build

If you are the team's lead and own main/prod, open an integrator member (`agents_open_tab`), hand it this skill and the feature, and stay on main. If you are the integrator, proceed.

You are the **integrator**: you design with the owner, write the team artifacts, open members, merge their lanes, make rulings and report. Members build; you stay lean and delegate deep reads and code to subagents.

Templates live beside this file in `templates/`. Copy them; don't rewrite them from memory.

Requires herdr+. If `agents_whoami` fails, stop and tell the owner this skill needs a herdr+ team.

## Non-negotiable (ranked by seat-hours lost in one build when skipped; §9.4 re-ranks after each retro)
1. **T0 artifacts tracked on the integration branch before any lane starts**: spec, `rulings.md`, `ownership.md`, `seams.md`. **Rulings: file first, message second.** Commit `R-NNN` to `rulings.md`, then send a pointer (`R-NNN: <one line>. Affects you: <action|none>`). Members re-read it before every subagent launch and every hand-off, and read herdr after every subagent return. Hand-offs are pull-based: a row in `handoffs.md` + a one-line pointer; the integrator polls the file.
2. **Stable tips.** The integrator merges ready hand-offs in windows (30–45 min, or at once when a merge unblocks a lane), runs the gate, then announces ONE `TIP <sha>`. Lanes rebase only onto announced tips and verify linearity themselves before hand-off. Full gates run only on an announced sha; nothing merges while that gate runs.
3. **Shared base before fan-out.** Land AND audit the shared base (shell, fixtures, harness setup, test config, consumer side of the contract) first; lanes branch only from the integration tip. Each shared file has one CONTENT owner plus named secondary writers per wave in `ownership.md`; foreign edits go in a separate flagged commit; whoever changes a signature edits its callers mechanically and the file owner rebases.
4. **Real-module harness at freeze.** Tests wires the real modules together (only I/O faked) with knobs: producer identity change mid-life, REWIND (restore drops the last N records and renumbers), old producer build without the new headers. Every lane built on a stand-in passes it before its first cross-review.
5. **Contracts have consumers.** `seams.md` lists every value produced in one lane and consumed in another (producer file → consumer file → test); a field with no named consumer is dropped. A post-freeze change to a frozen shape is a numbered ruling naming each consumer owner and the behaviour change, and lands with their tests red→green in one window.
6. **Disaster walks at freeze, drills from the first runnable slice.** The architect walks THIS build's failure scenarios through BOTH sides of every contract (producer answer → consumer merge) and every timer/actor. Drill scripts are written at freeze, dry-run as `NEEDS <WP>`, and run on every merge batch.
7. **Every test run goes through the deadline wrapper** (deadlines ≈ 2× a measured baseline, heartbeat ≤ 60 s). Lanes run targeted tests only; one owner runs full gates, serialized per machine. Exit codes from files; 0 tests ran = FAIL; a producer that can exit 0 on bad output is not a gate.
8. **Seats follow the work, ruled at T0.** Drills, inspectors and UI review open at T0 whenever the work has their subject; the seat list is a T0 ruling (§2). One full-branch read of the EXACT ship sha by the inspectors before ship. Only the owner may drop it or any ruled-in review step.
9. **Skip ratchet + ship gate from day one.** The ratchet counts per parametrised case across every language, including conditional skip decorators. The gate FAILS, never skips, runs on an export, and is proven once on a simulated all-lanes-merged tree.
10. **Owner-only decisions**: the list in §7 (one place). If the owner is away, decide provisionally, mark `[PROVISIONAL]` in `rulings.md` when made, and list each in the GO report.

## 0. Before anything
- `agents_whoami`. Know your team, the lead, and who owns main and prod.
- Read the repo's CLAUDE.md, its conventions for branches/worktrees/commits, its paid-call/cost rules (if any), and every gate per toolchain plus any cross-language contract check (e.g. regenerate a client, then diff). For each gate record the command, how its exit code is read (file, never a pipe) and its output-validity check (e.g. the generated spec parses). Measure each gate once on the clean tip; deadline = 2× measured, STALL_S ≥ the slowest silent phase. Note which audit tools exist (cross-model CLI, multi-agent audit); with no second model, run 2 independent Opus auditors with different lenses + 1 judge. Name the ship mechanism: deploy (service), release (binary/package) or merge-only; every "deploy" below means that step.

## 1. Design with the owner
1. Map today's system first, as a **citable `path:line` index** plus a short owner summary. When the map names a guard, trace where the guard's input comes from. A separate mapper member is fine, and it stays on call for the designer and the lanes. Measure as well as read (payload size, latency, req/min on a local rig). Output a failure-mode table: each known bug as a timeline, covered / not covered by existing fixes. Quick wins ship separately, before the redesign, with their own gates. The mapper is independent of the code's authors and read-only; the designer sends it a verify-these-claims list.
2. Write an options doc: 2–3 designs, trade-offs, your recommendation. **Lead with the simplest end state**, e.g. a single cutover rather than phased modes.
3. Ask the owner early: "what would you give up?" The answer gives you the must-keeps.
4. Real forks go through AskUserQuestion: concrete options, a recommended one first, the consequence of each stated. Never ask about things the code or the spec already answers.
5. Scope: when scope is contested, run two advocates (lean vs complete) and a judge that checks every load-bearing claim against the code.
6. Estimate by the **critical path of rulings and merges**, then ask of every serial link whether it can split behind a frozen shared base. One data point: a scope judge's 416 h / 8–10 working days became ~11 h wall from freeze to FF-ready once the single-editor cutover chain was split into parallel lanes.
7. Write the RULED spec. It is tracked in git from its first version (§2).

## 2. Team setup (T0), before any member starts
Create these on the integration branch, tracked, in ONE spec directory (`<specs>/<slug>/`); `findings.md` and `handoffs.md` live in the scratch team dir:

| File | Contents | Template |
|---|---|---|
| `<slug>.md` | ruled spec, `Status:` line | — |
| `rulings.md` | numbered rulings `R-001…`, append-only, each dated. It overrides the spec where they differ | `templates/rulings.md` |
| `ownership.md` | (a) files/dirs per member, incl. test fixtures; (b) **acceptance criterion → owner → test name** for every behaviour, including behaviours that span two members' files; (c) shared files and their merge order (e.g. who regenerates generated types last); (d) who merges into the lane, who FFs to main, who bumps the version, who deploys, who is told at FF time (the main-tree owner); (e) seats → members (one member may hold several non-lane seats, see §2a) | `templates/ownership.md` |
| layout | the target folder for every new file, tests named by subject (never by lane, WP letter or audit round) | in `ownership.md` |
| `seams.md` | every value produced in one lane and consumed in another: producer file → consumer file → owning test; a field with no consumer is dropped before freeze | `templates/seams.md` |
| `findings.md` | created by the integrator at T0 (scratch); columns id, layer, severity, branch@sha, file:line, owner, STATE | — |
| `handoffs.md` | append-only hand-off rows (WP ids, branch@sha, base TIP, rulings ≤ R-NNN, targeted exit codes, audit summary, foreign-file commits) | — |

Names: **key = member**; one worktree per member (`../<repo>-<slug>-<member>`); one scratch dir per member (`<scratch>/<slug>/<member>/`). Use the repo's branch prefix if it has one, else `lane/`: `<prefix><slug>-<member>[-<wp>]`; fixer/integration branches `<prefix><slug>-<wp>-<purpose>`, never audit-round names. Every branch and worktree is **registered when created** in `ownership.md` §Refs (owner, purpose, base, close-out step); an unregistered ref is a finding. Cap: one worktree per active member plus one per running parallel fixer; a fixer's worktree is removed when its commits land. At each wave boundary delete superseded refs (`git cherry` empty, `branch -d`) and report the ref and worktree counts.

Roles: pick the seats this build needs from the work and record the seat list as a ruling at T0. Drills, inspectors and UI review open at T0 whenever the work has their subject (e.g. no UI reviewer when nothing user-facing changes; no monitor when nothing ships). Dropping a ruled-in seat later is owner-only (§7).

| Role | Job | Brief |
|---|---|---|
| integrator (you) | rulings, merges, tip announcements, owner reports | — |
| lane members (1 per coherent area; usually 2–6) | thin orchestration of Opus subagents; written pre-freeze task if opened before the freeze (path:line map of own area, contract asks, test list; no worktree, no code until "contracts frozen @ <sha>") | `templates/team-brief.md` |
| tests | real-module harness ahead of code (§3.4b); tagged pending skips per acceptance row; skip ratchet; ship gate script; known-flake list; full gates | team brief + §3.4, §5 |
| drills | the real artifacts as users run them (browser for a web client; for a CLI/service: start the real service, run the built binary incl. one from the previous release, kill/restart/restore mid-operation, assert on rows + logs + exit codes). Scripts at freeze, dry-run `NEEDS <WP>`; runs on every merge batch. Owns the runbook (others send edits) | `templates/drills-brief.md` |
| inspector-1 | follows one edit end to end, across lane seams | `templates/inspector-brief.md` |
| inspector-2 | shapes between lanes, plus deploy/rollback/gates. Runbook and deploy scripts count as code. Contract checks from the first lane commits; cutover checks after the first merges. Each pass also builds a throwaway merge of the tip + every live lane tip and runs the ship gate + runbook greps on it | `templates/inspector-brief.md` |
| architect | disaster walks at freeze through both sides of every contract; GO/NO-GO when each side's state core is complete (producer write path; consumer store/merge) and at final | §3.3, §6 |
| UI reviewer | user-visible surface review: the spec's copy + state table as ONE file (string, surface = chip/toast/banner or CLI flag/help/stderr/exit code or API error body, triggering state); then one pass on the first runnable build with every failure state forced by a stub flag | `templates/ui-brief.md` |
| monitor | after the ship step. If there is no running prod, the monitor = a post-release smoke on the published artifact | `templates/monitor-brief.md` |

- Write `templates/team-brief.md` into the tracked spec dir next to `rulings.md`; only its header block (STATE · TIP · last ruling · FINAL) changes in place, plus an append-only tip log. Members re-read the header before acting.
- If the build changes anything outside the shipped artifact (env, manifests, volumes, infra): at T0 the deployer hands over a read-only LIVE snapshot (deploy spec, env, image, volumes), diffed against the repo manifests; every such change becomes a named runbook step with an owner and a verify line. Always name the lane sha and the ship sha (version-bump commit) now.
- Check external prerequisites now (keys/accounts for any paid or external drill). Announce every change to a file owned outside the team (deploy script, infra, repo docs) to its owner before committing it.

### 2a. Small team (≤3 lanes)
Seats are duties. One member may hold several of the non-lane seats ruled in at T0 (e.g. tests + inspector-2; inspector-1 + drills; architect = one Opus subagent the integrator runs at freeze and at GO, writing its verdict to a file). A lane member never reviews its own lane; write the seat → member map in ownership.md. Cadence may shrink without a ruling (inspector timer → tip-triggered; tip pings only to affected members); ruled-in review LAYERS may not (walks, ratchet + gate, per-merge audit, full-branch read, drills, UI review) — dropping one is owner-only (§7).

## 3. Contract freeze
1. Freeze **shapes**: function signatures, DDL, wire/request/response shapes, client types, one regen of generated types.
2. Do **not** freeze lifecycle state machines (link loss, reload, rollback, "header absent means?"). Mark them "to be ruled", each with a named owner.
3. Have the architect run **disaster walks** against the contracts, through BOTH sides (producer answer → consumer merge) and every timer/actor, incl. the idle cases: restore from backup, rollback of each component, version skew both ways, two concurrent actors on one record, crash mid-write, retention vs every reader's cursor, leave/reopen during a pending op, local refusal on every press (e.g. for a browser sync build: takeover mid-flight, two tabs, sleep-through). Each walk forces a ruling now. Freeze the invariants as testable statements.
4. On day one, tests writes the structural gate tests (e.g. "every write route is wired") and one test per acceptance row. Each acceptance-row test is a **tagged pending skip** naming its owner. The skip ratchet (§5) counts it and the ship gate script reports it. Never use a bare skip, and never leave a bare red that breaks everyone's gate. A behaviour with no owner shows up as an untagged row. Structural gate tests are the exception: they are RED inside the ship gate only, from day one.

   4b. Tests wires the **real-module harness** at freeze (box 4). Knobs from T0: producer identity change mid-life; REWIND (restore drops the last N records and renumbers); old build without the new headers; independent clocks per retry loop. The harness proves logic, ordering and races; drills prove which build/process answers, real platform APIs and kill -9 / ship gaps.
5. **Shared base first.** For any file several lanes need (shell, shared context, test fixtures, harness setup, test config), freeze its interface, land and audit the base (incl. the consumer side of the contract) before fan-out. One CONTENT owner per file; secondary writers per wave named in ownership.md; the signature-changer edits callers mechanically and the file owner rebases. Lanes starting on an unmerged base cost about as many hours in rebases and bridges as the parallel start buys.
6. Announce "contracts frozen @ <sha>" in one message to all members.
7. Write `seams.md` (box 5) and hand it to inspector-1.

## 4. Fan out
The team brief (`templates/team-brief.md`) carries these hard rules from minute one:
- Run every test through the deadline + stall-kill wrapper (`templates/run-with-deadline.sh`). Deadlines per suite in ownership.md ≈ 2× a measured baseline (starting points: one file 300 s, targeted 600 s, full gate 1500 s). Long suites print a heartbeat ≤ 60 s or raise STALL_S; commands that buffer output (e.g. `go test` without `-v/-json`) need STALL_S raised. A second hang is a finding, and the file becomes a tagged pending skip.
- Lanes run **targeted** tests only. The integrator (or the tests member) owns the full gates, one run per merge batch. Cap test workers; serialize full gates per machine.
- Own worktree per member, and one worktree per parallel fixer subagent. Pre-commit hooks stash unstaged files, so two fixers must never share a worktree.
- Branch only from the integration tip. Right before each hand-off: rebase onto the latest ANNOUNCED tip, verify linearity yourself (`git merge-base --is-ancestor`), check content with `git cherry`, and rerun targeted tests after the rebase. Never rebase a branch the integrator has already taken.
- Edits to a file another member owns go in a separate commit, flagged in the hand-off. Better still, message the owner.
- **Read points**: before every subagent launch, before every hand-off, after every subagent return. Read = `git diff <last-read-sha> <integration> -- rulings.md` (never your worktree copy). Every subagent prompt and every hand-off carries the stamp `TIP <sha> · rulings ≤ R-NNN`; the integrator returns a hand-off whose stamp misses a ruling that affects it.
- Subagents: give self-contained prompts, write first, commit each finished item, and never report "done" with uncommitted edits. A scope change restates the FULL final list, never a delta. Subagents report only to their member, never to the integrator or reviewers directly. Subagent prompts say "verify the premise against the current tip first". A finished subagent never reads messages: relaunch with a self-contained prompt stating done/next, never "continue".
- Kill only your own processes, by PID or process group.
- Messages carry pointers, verdicts, questions and ruling requests. Files carry rulings, TIP/state, hand-off rows, findings, drill status and anything > 4000 chars. Never send status a file already shows. First sends may fail `caller_unresolved` for ~1 min: retry after `agents_whoami` succeeds. A queued send is not lost; do not resend. Lane↔lane direct messages are fine for shapes and repros; rulings and reassignments only through the integrator.

## 5. Run the build (integrator loop) — per merge: `templates/merge-wave-checklist.md`
- **Merge** ready hand-offs (from `handoffs.md`) in windows: default 30–45 min, or at once when a merge unblocks a waiting lane. Gate the result on a frozen sha in a dedicated clean gate worktree (never the merge worktree), then announce ONE `TIP <sha>` per window with what landed and who must rebase. Linearity is checked against the announced tip the lane rebased onto; if the head moved by docs/rulings commits or disjoint files, the integrator rebases `--onto`, confirms `git range-diff` is patch-identical, and merges; only real conflicts go back. Never FF a commit nobody handed off.
- **Rulings**: when a member raises a deviation, it should send a failure timeline, one proposed rule, and "object or I proceed". Rule within minutes. Append the ruling to `rulings.md` (tracked) and broadcast it to every affected member with sha and exact shape. Reassign work once, with the full final list. A ruling that changes a frozen shape or semantic names each consumer (member + file + test), states the behaviour change, goes only to the named consumers, and their tests flip red→green in the same window. Batch such changes into scheduled drops.
- **Ownership drift**: when a ruling moves who edits what, update `ownership.md` in the same minute.
- **Skip ratchet**: tests keeps a counter of pending skips that can only go down. Count per parametrised case, across every language. The scanner is stdlib-only so the ship step can run it. It counts skip decorators statically, including conditional ones (`skipif(not FLAG)`), which are deleted at the flip. Raising the max needs a logged ruling (honest recounts do raise it).
- **Ship gate**: a script (the feature's activation switch — flag, config or release tag — in its ship state, ratchet 0) that the ship step runs on a `git archive` export. The gate FAILS, never skips; prove it at T0 on a tree with one pending skip per language (expect CLOSED). The integrator runs it on each announced tip; inspector-2 also runs it on a throwaway all-lanes-merged tree each pass. **Honest gates**: a step counts only if its producer's exit code AND output validity are checked; no pipes, no `|| true`, no unquoted file-list vars; a module reachable from a CLI never prints to stdout.
- **Findings** go into `findings.md` (created by the integrator at T0): id, layer, severity, branch@sha, file:line, owner, STATE ∈ OPEN → ROUTED(owner) → FIXED@sha → VERIFIED@sha (only the finder moves it) | DEFERRED(R-id) | REJECTED(reason). Messages carry only verdicts. Drill status goes into a per-run summary file plus a drill × sha tracker row. **Flakes**: the gate runner keeps a known-flake list (test, symptom, first seen); a listed red is rerun alone once — green alone updates the row, red alone is a finding. Per-test hang guards go in at T0.
- **Reorg** only before fan-out or after cutover. A mid-build move-only step works only for an area whose lanes are quiet, and only with a deterministic script, identical generated API output, and identical before/after test counts.

## 6. Review checkpoints (all layers, each labeled)
Each layer sends ONE consolidated verdict per checkpoint, labelled `<layer>@<sha>: GO|NO-GO|MUST list`; reviewer subagents report only to their reviewer.

| When | Layer |
|---|---|
| per WP | the member's own Opus audit: cheap, catches in-lane bugs, blind to seams |
| each assembled batch before merge; a single-lane merge that touches a seams.md row gets the same audit | cross-model audit (2 Claude + Codex + judge) with focus items for seams ("every press under a local refusal", "leave/reopen during a pending op") |
| continuous | 2 inspectors triggered by every announced TIP and every new ruling (10-min timer as fallback); they re-verify each fix as it lands, because fixes create the next bugs |
| each side's state core complete (producer write path; consumer store/merge); final | architect GO/NO-GO, walks rerun through the code just completed. Freeze the tip for the review window, or send a delta list |
| scripts at freeze (dry-run `NEEDS <WP>`); every merge batch from the first runnable slice. A drill run names its target sha and is void if the tip moved; test a sha in a detached temp worktree | drills: assert outcomes (rows, storage, request log, server log). Never assert on timing or event order; detect the order and branch on it |
| before ship | **one full-branch read** (rename detection: `git diff -M --stat <base>...<ship sha>`, then per reviewer area) of the EXACT ship sha, by the inspectors as a fresh full read (not a delta stack). Only the owner may drop it. Commits after the last batch audit get a targeted seam re-audit or an inspector VERIFIED@sha |
| each suite before its first merge, and on every skip flip | a "break the precondition" audit: break the input or precondition and the test must go red; any `if present` guard on an asserted value is hollow; a flipped skip counts only with positive assertions |

## 7. Owner in the loop
- Owner-only decisions: user-visible behaviour, user-facing copy, risk acceptances (including dropping a planned gate, audit or ruled-in review seat), paid budgets, deploy GO. Everything else is yours. This list lives here only; box 10 and rulings.md point to it.
- Before the owner leaves: get a finish line, a paid-test cap, a deferral rule ("defer non-core if late; never data-safety; first candidate = X"), and, if the build makes paid calls, a cost-recording hook per CLAUDE.md and how paid smoke runs (keys checked at T0; agents never hunt for keys). Schedule the UI review.
- While the owner is away: make owner-only decisions provisionally, mark them `[PROVISIONAL]` in rulings.md when made, and list every one in the GO report. Never let the owner learn of one only after the fact.

## 8. Outages (usage limit, crashes)
- Plan for full parallelism; usage quotas are not a planning input unless the owner says so. Before long runs, report where things stand to the owner (agents_send_message to the lead/owner pane) and, if the owner asked for a notes page, update it.
- After a reset: resume subagents, never restart them (their uncommitted work is in the worktree). Do one catch-up pass and announce status, not a burst of stacked loop ticks. Re-plan against the finish line and name what was dropped. Resume = send the exact state (done/next), never "continue"; the integrator re-kicks every lane.
- Keep a "pause all" protocol: each member answers with its branch head and state.

## 9. Cutover and after
1. Pre-deploy: tested sha == FF sha, or the diff between them touches only the spec dir (rulings/spec re-snapshot) and is listed in the GO report; every code-bearing ruling's commit is an ancestor of the deploy sha (`merge-base --is-ancestor`); gate OPEN on an export; full gates on that sha; drills on the final client; all reviewer verdicts; runbook with verify lines + drain-first rollback (rollback = the inverse of the ship step). **FINAL is announced once**, carrying the rulings → commit map. Anything after it goes out as `FINAL+n <sha>: files, rulings closed, what each reviewer must re-check`, never a new FINAL. Before the FF: main tree linear and no untracked file on a path the lane adds.
2. Owner (or the named deployer) performs the ship step and tells the main-tree owner at FF time. Then open the monitor (or the post-release smoke), with `templates/monitor-brief.md` filled in first (ready checker, pinned values, thresholds as code). It is read-only, and alarms go to you.
3. Close out:
   - send a one-line report to lead;
   - stop your loops;
   - post a suspend roster (who is done, who to suspend);
   - **refs/worktrees gate — close-out FAILS while any registered ref or worktree of this build remains.** For each §Refs row: merged (`git cherry` empty) → archive runtime data + scratch first → `git worktree remove` (never `--force`) → `branch -d` (never `-D`); unmerged → its owner rules merge, archive or drop. Report `refs 0 · worktrees 0`. Removals in the shared main repo are run by the owner when the repo requires it;
   - **departures:** no member leaves or is suspended with open items. Each one hands its open queue items (post-ship follow-ups, DEFERRED rows, findings it owns, its §Refs rows) to a named owner, recorded in `ownership.md`; the suspend roster lists who took what;
   - remove the cutover scaffolding (gate script, ratchet, ship hook) in the FIRST commit after the ship is verified, never in the ship commit;
   - update the ledgers the main-tree owner keeps (spec Status: LIVE, deploy log, prod memory);
   - fold rulings.md into the spec now (the only time), then move the spec dir to the archive;
   - list main-tree untracked copies to delete and DEFERRED entries to open.
4. Run a short retro: interview members with `templates/interview-questions.md` and fold the lessons back into this skill. The interview asks every seat for a per-bucket % time split (building / fixing review findings / tests / waiting / rebase / comms / rework / outage) and its ONE change with an hours estimate; re-rank the box by those hours.
