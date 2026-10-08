#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=12

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session|subagent|stop|turn) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:claude"
action = os.environ.get("HERDR_ACTION", "")
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

if "CURSOR_VERSION" in os.environ or "cursor_version" in hook_input:
    raise SystemExit(0)
hook_event_name = str(hook_input.get("hook_event_name") or "")
request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"


def send(request):
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(0.5)
        client.connect(socket_path)
        client.sendall((json.dumps(request) + "\n").encode())
        try:
            client.recv(4096)
        except Exception:
            pass
        client.close()
    except Exception:
        pass


# Subagents running under the pane's agent (pane.report_subagent). A server
# without the method answers with an error, which is ignored.
if action == "subagent":
    subagent_events = {"SubagentStart": "start", "SubagentStop": "stop"}
    subagent_id = hook_input.get("agent_id")
    if hook_event_name not in subagent_events or not isinstance(subagent_id, str) or not subagent_id:
        raise SystemExit(0)
    # Claude's internal helper agents carry no agent_type; they are not work
    # the user started.
    agent_type = hook_input.get("agent_type")
    if not isinstance(agent_type, str) or not agent_type:
        raise SystemExit(0)
    send({
        "id": request_id,
        "method": "pane.report_subagent",
        "params": {
            "pane_id": pane_id,
            "agent": "claude",
            "event": subagent_events[hook_event_name],
            "subagent_id": subagent_id,
        },
    })
    raise SystemExit(0)

# A turn of the main agent started (UserPromptSubmit: a typed prompt, a task
# notification or a subagent's hand-back) or ended (Stop), for message
# delivery (pane.report_turn). Claude's prompt_id pairs the two; the hook's
# clock orders reports that cross on the socket.
def report_turn(event):
    params = {
        "pane_id": pane_id,
        "agent": "claude",
        "event": event,
        "seq": time.time_ns(),
    }
    prompt_id = hook_input.get("prompt_id")
    if isinstance(prompt_id, str) and prompt_id:
        params["prompt_id"] = prompt_id
    send({"id": request_id, "method": "pane.report_turn", "params": params})


if action == "turn":
    if hook_event_name == "UserPromptSubmit" and not hook_input.get("agent_id"):
        report_turn("start")
    raise SystemExit(0)

# The main agent's turn ended (Stop): report every subagent still running
# from Claude's background_tasks, background ones included, which replaces
# the pane's set. A Claude Code without the field reports nothing. Never
# from SubagentStop, where the subagent's own entry is still running.
if action == "stop":
    if hook_event_name != "Stop" or hook_input.get("agent_id"):
        raise SystemExit(0)
    report_turn("end")
    background_tasks = hook_input.get("background_tasks")
    if not isinstance(background_tasks, list):
        raise SystemExit(0)
    subagent_ids = []
    for task in background_tasks:
        if not isinstance(task, dict) or task.get("type") != "subagent":
            continue
        task_id = task.get("id")
        agent_type = task.get("agent_type")
        if isinstance(task_id, str) and task_id and isinstance(agent_type, str) and agent_type:
            subagent_ids.append(task_id)
        if len(subagent_ids) >= 64:
            break
    send({
        "id": request_id,
        "method": "pane.report_subagent",
        "params": {
            "pane_id": pane_id,
            "agent": "claude",
            "event": "snapshot",
            "subagent_ids": subagent_ids,
        },
    })
    raise SystemExit(0)

if hook_event_name != "SessionStart":
    raise SystemExit(0)
is_subagent = bool(hook_input.get("agent_id"))
if is_subagent:
    raise SystemExit(0)
report_seq = time.time_ns()
session_id = hook_input.get("session_id")
agent_session_id = session_id if isinstance(session_id, str) and session_id else None
transcript_path = hook_input.get("transcript_path")
agent_session_path = transcript_path if isinstance(transcript_path, str) and transcript_path else None
session_start_source = hook_input.get("source") if hook_event_name == "SessionStart" else None
if not isinstance(session_start_source, str) or not session_start_source:
    session_start_source = None
if agent_session_id:
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "claude",
        "seq": report_seq,
        "agent_session_id": agent_session_id,
    }
    if agent_session_path:
        params["agent_session_path"] = agent_session_path
    if session_start_source:
        params["session_start_source"] = session_start_source
    request = {
        "id": request_id,
        "method": "pane.report_agent_session",
        "params": params,
    }
else:
    raise SystemExit(0)

send(request)
PY
