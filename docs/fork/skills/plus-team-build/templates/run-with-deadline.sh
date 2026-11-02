#!/usr/bin/env bash
# Run a test command with a hard deadline and a stall check; kill its whole process group on either.
# Usage: run-with-deadline.sh <max_seconds> <log_file> <cmd...>
#   - Output goes to <log_file>; exit code = the command's, or 124 on deadline, 125 on stall.
#   - Stall = the log file has not grown for STALL_S seconds (default 300).
#   - Commands that buffer output (go test without -v/-json, non-TTY reporters) need STALL_S raised; size deadlines at ~2× a measured baseline.
# Portable (macOS has no `timeout`); also reaps vitest/xdist workers (no orphans).
set -u
max=$1; log=$2; shift 2
stall=${STALL_S:-300}
: > "$log"
set -m
"$@" > "$log" 2>&1 &
pid=$!
pgid=$(ps -o pgid= -p "$pid" | tr -d ' ')
start=$(date +%s); last_size=0; last_change=$start
reason=""
while kill -0 "$pid" 2>/dev/null; do
  sleep 10
  now=$(date +%s)
  size=$(stat -c %s "$log" 2>/dev/null || stat -f %z "$log" 2>/dev/null || echo 0)
  if [ "$size" != "$last_size" ]; then last_size=$size; last_change=$now; fi
  if [ $((now - start)) -ge "$max" ]; then reason="deadline ${max}s"; break; fi
  if [ $((now - last_change)) -ge "$stall" ]; then reason="stalled ${stall}s (no output)"; break; fi
done
if [ -n "$reason" ]; then
  kill -TERM -- "-$pgid" 2>/dev/null; sleep 5; kill -KILL -- "-$pgid" 2>/dev/null
  echo "run-with-deadline: KILLED ($reason): $*" | tee -a "$log" >&2
  [ "${reason#deadline}" != "$reason" ] && exit 124 || exit 125
fi
wait "$pid"; exit $?
