#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=17

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session|subagent|stop|turn|precompact|browser) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0
# The browser steering (PreToolUse) runs only in launches `herdr agent wrap`
# marked, and skips python for tool calls that cannot open a page.
if [ "$action" = "browser" ]; then
  [ -n "${HERDR_BROWSER_STEER_STATE:-}" ] || exit 0
  [ -n "${HERDR_BROWSER_STEER_CONFIG:-}" ] || exit 0
  grep -Eq 'WebFetch|mcp__|open|webbrowser|osascript|sensible-browser|x-www-browser' "$hook_input_file" 2>/dev/null || exit 0
fi

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import hashlib
import json
import os
import random
import re
import shlex
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


# herdr+ browser steering (PreToolUse): a WebFetch, a shell command that
# opens a web page or a local HTML file in another browser, or another
# browser automation MCP server is denied once with what to use instead
# (the herdr-browser tools). The same call again within STEER_RETRY_SECONDS
# runs (the user asked for it); each allowed retry keeps the window open.
# Only in launches `herdr agent wrap` marked (HERDR_BROWSER_STEER_STATE and
# _CONFIG), and never with `[browser] enabled = false`, `steer_agents =
# false` or no herdr-browser server in Claude's user-scope MCP store. Any
# error allows the call.
STEER_RETRY_SECONDS = 120
BROWSER_MCP_SERVER = re.compile(
    r"playwright|puppeteer|chrome|browsermcp|browserbase|browser-use|browser_use", re.I
)
OPEN_LAUNCHERS = {"xdg-open", "sensible-browser", "x-www-browser", "gnome-open", "kde-open", "wslview"}
# `open` options that edit or reveal the file instead of viewing it.
OPEN_NOT_VIEWING = {"-e", "-t", "-f", "-R", "--reveal", "-h", "--header"}
OPEN_VALUE_OPTIONS = {"-a", "-b", "--env", "--stdin", "--stdout", "--stderr"}
COMMAND_WRAPPERS = {"command", "exec", "nohup", "env"}


def web_target(arg):
    lower = arg.lower()
    if lower.startswith(("http://", "https://", "file://")):
        return True
    path = re.split(r"[?#]", arg, maxsplit=1)[0].lower()
    return path.endswith((".html", ".htm"))


def shell_commands(text):
    for line in text.splitlines():
        try:
            lexer = shlex.shlex(line, posix=True, punctuation_chars=True)
            lexer.whitespace_split = True
            lexer.commenters = ""
            tokens = list(lexer)
        except ValueError:
            continue
        current = []
        for token in tokens:
            if token.startswith("#") and not current:
                break
            if token and set(token) <= set(";&|()"):
                if current:
                    yield current
                current = []
            else:
                current.append(token)
        if current:
            yield current


def first_web_positional(args, value_options=()):
    skip = False
    for arg in args:
        if skip:
            skip = False
            continue
        if arg in value_options:
            skip = True
            continue
        if arg.startswith("-"):
            continue
        if web_target(arg):
            return arg
    return None


def shell_open_target(text):
    for words in shell_commands(text):
        while words and (
            re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", words[0]) or words[0] in COMMAND_WRAPPERS
        ):
            words = words[1:]
        if not words:
            continue
        name = os.path.basename(words[0])
        args = words[1:]
        target = None
        if name == "open":
            if "--args" in args:
                args = args[: args.index("--args")]
            if not any(arg in OPEN_NOT_VIEWING for arg in args):
                target = first_web_positional(args, OPEN_VALUE_OPTIONS)
        elif name in OPEN_LAUNCHERS:
            target = first_web_positional(args)
        elif name == "gio" and args[:1] == ["open"]:
            target = first_web_positional(args[1:])
        elif re.match(r"^python[0-9.]*$", name) and "-m" in args:
            at = args.index("-m")
            if args[at + 1 : at + 2] == ["webbrowser"]:
                target = first_web_positional(args[at + 2 :])
        elif name == "osascript":
            found = re.search(r"open location\s+\"([^\"]+)\"", " ".join(args))
            if found and web_target(found.group(1)):
                target = found.group(1)
        if target:
            return target
    return None


def browser_call(tool_name, tool_input):
    if tool_name == "WebFetch":
        url = tool_input.get("url")
        return ("fetch", url) if isinstance(url, str) and url else None
    if tool_name.startswith("mcp__"):
        parts = tool_name.split("__")
        server = parts[1] if len(parts) >= 3 else ""
        if server and server != "herdr-browser" and BROWSER_MCP_SERVER.search(server):
            return ("mcp", server)
        return None
    if tool_name == "Bash":
        command = tool_input.get("command")
        if isinstance(command, str):
            target = shell_open_target(command)
            if target:
                return ("open", target)
    return None


# `[browser] enabled` or `steer_agents` set to false (a line scan: the
# values are plain booleans). A missing file is the defaults (both on).
def browser_steering_off(config_path):
    try:
        with open(config_path, encoding="utf-8") as handle:
            text = handle.read()
    except FileNotFoundError:
        return False
    section = ""
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        header = re.match(r"^\[\s*([^\[\]]+?)\s*\]$", line)
        if header:
            section = header.group(1).replace(" ", "")
            continue
        if line.startswith("["):
            section = "\0"
            continue
        pair = re.match(r"^([A-Za-z0-9_.\-]+)\s*=\s*(\S+)$", line)
        if not pair:
            continue
        key = pair.group(1) if not section else section + "." + pair.group(1)
        if key in ("browser.enabled", "browser.steer_agents") and pair.group(2) == "false":
            return True
    return False


def herdr_browser_registered():
    claude_dir = os.environ.get("CLAUDE_CONFIG_DIR")
    store = (
        os.path.join(os.path.expanduser(claude_dir), ".claude.json")
        if claude_dir
        else os.path.join(os.path.expanduser("~"), ".claude.json")
    )
    with open(store, "rb") as handle:
        return b'"herdr-browser"' in handle.read()


# True when this call is the user's retry (a denial for the same session
# and target within the window; the window restarts). Otherwise records the
# denial and answers False; an error raises, which allows the call.
def steer_retried(state_dir, session, target):
    os.makedirs(state_dir, exist_ok=True)
    key = hashlib.sha256(f"{session}\0{target}".encode("utf-8", "replace")).hexdigest()[:32]
    marker = os.path.join(state_dir, key)
    now = time.time()
    try:
        recent = now - os.stat(marker).st_mtime <= STEER_RETRY_SECONDS
    except FileNotFoundError:
        recent = False
    with open(marker, "w", encoding="utf-8") as handle:
        handle.write(str(int(now)))
    if recent:
        return True
    for name in os.listdir(state_dir):
        path = os.path.join(state_dir, name)
        try:
            if path != marker and now - os.stat(path).st_mtime > STEER_RETRY_SECONDS:
                os.remove(path)
        except OSError:
            pass
    return False


def steer_reason(kind, target):
    retry = (
        " Only if the user asked for this, run the same call again within 2 minutes"
        " and it will be allowed."
    )
    if kind == "fetch":
        return (
            f"herdr+: read web pages with the herdr browser, not WebFetch: browser_open {target}"
            " then browser_read (or browser_find), herdr-browser MCP." + retry
        )
    if kind == "mcp":
        return (
            f"herdr+: use the herdr-browser tools (browser_open, browser_read, browser_snapshot,"
            f" browser_click, browser_fill, browser_screenshot, ...) instead of {target}; the herdr"
            " browser is the one the user watches and signs in to." + retry
        )
    if target.lower().startswith(("http://", "https://")):
        return (
            f"herdr+: open {target} in the herdr browser, which the user sees: browser_open {target}"
            " (browser_focus brings it to the front), then browser_read to read it, herdr-browser"
            " MCP." + retry
        )
    path = target
    if not path.lower().startswith("file://") and not os.path.isabs(path):
        cwd = hook_input.get("cwd")
        if isinstance(cwd, str) and cwd:
            path = os.path.normpath(os.path.join(cwd, path))
    return (
        f"herdr+: show local HTML to the user with browser_show (herdr-browser MCP; file: {path});"
        " to read it yourself, browser_open it then browser_read." + retry
    )


def steer_browser():
    if hook_event_name != "PreToolUse":
        return
    state_dir = os.environ.get("HERDR_BROWSER_STEER_STATE")
    config_path = os.environ.get("HERDR_BROWSER_STEER_CONFIG")
    tool_name = hook_input.get("tool_name")
    tool_input = hook_input.get("tool_input")
    if not state_dir or not config_path or not isinstance(tool_name, str):
        return
    if not isinstance(tool_input, dict):
        return
    call_ = browser_call(tool_name, tool_input)
    if call_ is None:
        return
    kind, target = call_
    if browser_steering_off(config_path) or not herdr_browser_registered():
        return
    session = hook_input.get("session_id")
    session = session if isinstance(session, str) else ""
    if steer_retried(state_dir, session, f"{kind}:{target}"):
        return
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": steer_reason(kind, target),
        }
    }))
    sys.stdout.flush()


if action == "browser":
    try:
        steer_browser()
    except Exception:
        pass
    raise SystemExit(0)

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
