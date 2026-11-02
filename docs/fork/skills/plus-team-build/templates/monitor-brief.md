# Brief — <slug> monitor (post-deploy)

<version> (<sha>) went live at <time>. Watch real usage, READ-ONLY, and raise alarms early. Report to **<integrator>**.

## Given to you ready (the integrator fills these in before opening you)
- Checker: `<path>/check.sh`. It runs a rolling <15>-min window and prints counters for every signal below, plus restarts and the health endpoint.
- Exact event names per signal, copied from the repo's observability doc; paid work carries a start/settle attempt id.
- Pinned expected values: version <v>, build sha <sha>, <identity values the build emits, e.g. schema version>.
- Thresholds as code, in the checker. The table below is a summary.

| Signal | Healthy | Alarm |
|---|---|---|
| | | |

## Load
CPU vs limit, memory, throttle as per-check delta from a local state file, disk, request p95 + rate.

## Slow-write context
What else ran; scheduled job windows.

## Statistics
p95 only at n ≥ 20 and sustained; print the sorted list; exclude long-lived streams.

## Session-only loops
Crons/loops die with the session; recreate after restart; escalation rule agreed with the integrator.

## Rules
- Read-only on prod; never touch the DB, never read secrets. Run every long command through the deadline wrapper.
- Check every ~5 min for 2 h, then every 15 min. Log one line per check in herdr notes (section Log), and post the usage shape hourly.
- Alarm = one message to the integrator: numbers, a 2-line read, and the evidence path. If it looks like data loss, put that in the first line.
- Rollback is the owner's decision, drain-first.
