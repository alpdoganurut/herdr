#!/bin/sh
# Stand-in for `claude` in the coordinator fork smoke tests
# (src/server/headless/tests/fork_smoke/coordinator.rs). Never installed,
# never put on a real PATH: the tests run it against a stand-in socket and
# replay the request it sent into the in-process server.
#
# Screen detection never identifies this script as Claude, so it reports
# the way a hook-integrated agent does (as news_run.py does): one
# `pane.report_agent` over HERDR_SOCKET_PATH for HERDR_PANE_ID, under its
# own hook source, with the agent label `claude`. The coordinator's phase
# machine keys off the managed-agent launch state that `start_agent` began
# and these reports settle.
#
# Usage: stub_claude.sh <idle|working|blocked> [claude argv...]
# `--session-id <id>` / `--resume <id>` in the argv is reported as the
# session id. HERDR_STUB_SEQ sets the per-source sequence (default: now in
# ms); the tests pass it so replays stay ordered.
set -eu

state="${1:-idle}"
[ $# -gt 0 ] && shift
session=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id | --resume)
      [ $# -gt 1 ] || break
      session="$2"
      shift 2
      ;;
    *) shift ;;
  esac
done

HERDR_STUB_STATE="$state" HERDR_STUB_SESSION="$session" python3 - <<'PY'
import json
import os
import socket
import time

sock = os.environ.get("HERDR_SOCKET_PATH")
pane = os.environ.get("HERDR_PANE_ID")
if not (sock and pane):
    raise SystemExit(0)
seq = int(os.environ.get("HERDR_STUB_SEQ") or int(time.time() * 1000))
params = {
    "pane_id": pane,
    "source": "test:claude-stub",
    "agent": "claude",
    "state": os.environ["HERDR_STUB_STATE"],
    "seq": seq,
}
session = os.environ.get("HERDR_STUB_SESSION")
if session:
    params["agent_session_id"] = session
request = {"id": "claude-stub:%d" % seq, "method": "pane.report_agent", "params": params}
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.settimeout(2)
client.connect(sock)
client.sendall((json.dumps(request) + "\n").encode())
client.close()
PY
