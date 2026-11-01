#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=16

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session|subagent|stop|turn|precompact) ;;
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
import sys
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


# One request whose answer is read in full (a line of JSON), within `timeout`
# seconds; None on any failure.
def call(payload, timeout=3.0):
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(timeout)
        client.connect(socket_path)
        client.sendall((json.dumps(payload) + "\n").encode())
        data = b""
        while not data.endswith(b"\n") and len(data) < 262144:
            chunk = client.recv(65536)
            if not chunk:
                break
            data += chunk
        client.close()
        return json.loads(data.decode("utf-8", "replace") or "null")
    except Exception:
        return None


# A compaction starts (PreCompact) or the compacted session starts
# (SessionStart, source compact), for herdr's handoff, then compact
# (pane.report_compact): herdr saves the transcript and a handoff before a
# compaction it did not start, and answers the compacted session's start with
# the handoff to read. A server without the method answers with an error.
def report_compact(event):
    params = {"pane_id": pane_id, "agent": "claude", "event": event}
    for key in ("session_id", "transcript_path"):
        value = hook_input.get(key)
        if isinstance(value, str) and value:
            params[key] = value
    trigger = hook_input.get("trigger")
    if isinstance(trigger, str) and trigger:
        params["trigger"] = trigger
    return call({"id": request_id, "method": "pane.report_compact", "params": params})


# Quiet messages other agents stored for this pane instead of typing them in
# (agents.take_quiet): herdr answers them as one text, framed as data, with
# the ids it offered; they are delivered once acknowledged (ack_quiet, after
# the text was printed). A server without the method answers with an error,
# which adds nothing.
def take_quiet():
    response = call(
        {"id": request_id, "method": "agents.take_quiet", "params": {"caller_pane": pane_id}},
        timeout=2.0,
    )
    result = response.get("result") if isinstance(response, dict) else None
    if not isinstance(result, dict):
        return None, []
    context = result.get("context")
    ids = result.get("ids")
    ids = [i for i in ids if isinstance(i, str)] if isinstance(ids, list) else []
    return (context if isinstance(context, str) and context else None), ids


def ack_quiet(ids):
    if ids:
        call(
            {"id": request_id, "method": "agents.ack_quiet",
             "params": {"caller_pane": pane_id, "ids": ids}},
            timeout=2.0,
        )


# Print the texts to add to the agent's context for `event`, as one object;
# True once it is written.
def add_context(event, parts):
    parts = [part for part in parts if part]
    if not parts:
        return False
    try:
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": "\n\n".join(parts),
            }
        }))
        sys.stdout.flush()
        return True
    except Exception:
        return False


if action == "precompact":
    if hook_event_name == "PreCompact" and not hook_input.get("agent_id"):
        report_compact("pre")
    raise SystemExit(0)

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
        quiet_context, quiet_ids = take_quiet()
        if add_context("UserPromptSubmit", [quiet_context]):
            ack_quiet(quiet_ids)
    raise SystemExit(0)

# The main agent's turn ended (Stop): report every subagent still running
# from Claude's background_tasks, background ones included, which replaces
# the pane's set. A Claude Code without the field reports nothing. Never
# from SubagentStop, where the subagent's own entry is still running.
# Every other in-flight entry (a shell, a monitor, a workflow, any type)
# and every session cron (CronCreate, ScheduleWakeup, /loop; `cron:<id>`)
# goes in background_task_ids: exiting would stop them, so herdr does not
# suspend the agent while any is listed.
if action == "stop":
    if hook_event_name != "Stop" or hook_input.get("agent_id"):
        raise SystemExit(0)
    report_turn("end")
    background_tasks = hook_input.get("background_tasks")
    if not isinstance(background_tasks, list):
        raise SystemExit(0)
    subagent_ids = []
    background_task_ids = []
    for task in background_tasks:
        if not isinstance(task, dict):
            continue
        task_id = task.get("id")
        if not isinstance(task_id, str) or not task_id:
            continue
        if task.get("type") != "subagent":
            if len(background_task_ids) < 64:
                background_task_ids.append(task_id[:128])
            continue
        agent_type = task.get("agent_type")
        if isinstance(agent_type, str) and agent_type and len(subagent_ids) < 64:
            subagent_ids.append(task_id)
    session_crons = hook_input.get("session_crons")
    if isinstance(session_crons, list):
        for cron in session_crons:
            cron_id = cron.get("id") if isinstance(cron, dict) else None
            if isinstance(cron_id, str) and cron_id and len(background_task_ids) < 64:
                background_task_ids.append(("cron:" + cron_id)[:128])
    params = {
        "pane_id": pane_id,
        "agent": "claude",
        "event": "snapshot",
        "subagent_ids": subagent_ids,
    }
    if background_task_ids:
        params["background_task_ids"] = background_task_ids
    send({
        "id": request_id,
        "method": "pane.report_subagent",
        "params": params,
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

context_parts = []
# The compacted session starts: add herdr's handoff to its context.
if session_start_source == "compact":
    response = report_compact("post")
    result = response.get("result") if isinstance(response, dict) else None
    context = result.get("context") if isinstance(result, dict) else None
    if isinstance(context, str) and context:
        context_parts.append(context)
# Any session start: the quiet messages stored for this pane.
quiet_context, quiet_ids = take_quiet()
context_parts.append(quiet_context)
if add_context("SessionStart", context_parts) and quiet_context:
    ack_quiet(quiet_ids)
PY
