from __future__ import annotations

import re
import unittest
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
HOT_PATH_SOURCES = (
    PROJECT_ROOT / "src" / "ui.rs",
    *sorted((PROJECT_ROOT / "src" / "ui").rglob("*.rs")),
    PROJECT_ROOT / "src" / "server" / "render_stream.rs",
)
APP_SERVER_SOURCES = (
    *sorted((PROJECT_ROOT / "src" / "app").rglob("*.rs")),
    *sorted((PROJECT_ROOT / "src" / "server").rglob("*.rs")),
)
TEST_MODULE = re.compile(r"(?m)^#\[cfg\(test\)\]\s*\nmod\s+\w+\s*\{")
INPUT_STATE_CALL = re.compile(r"(?:\.|::)input_state\b")
KEYBOARD_STATE_ANSI_CALL = re.compile(
    r"(?:\.|::)(?:keyboard_state_ansi|kitty_keyboard_state_ansi)\b"
)
AGGREGATE_STATE_CALLS = (
    (INPUT_STATE_CALL, "aggregate terminal input state; add a narrow accessor"),
    (KEYBOARD_STATE_ANSI_CALL, "formatted keyboard state"),
)
FORBIDDEN_CALLS = (
    *AGGREGATE_STATE_CALLS,
    (
        re.compile(r"(?:\.|::)screen_text_snapshot\b"),
        "formatted terminal screen snapshot",
    ),
    (
        re.compile(r"\bforeground_job\s*\("),
        "process-tree inspection",
    ),
)


# Fork (perf watch): the always-on perf counters stay lock-free and
# allocation-free where they run per frame or per loop iteration, and per-byte
# and per-pane code calls none of them except the per-hold sync counters.
PERF_SOURCES = tuple(
    path
    for path in sorted((PROJECT_ROOT / "src" / "perf").rglob("*.rs"))
    if path.name != "tests.rs"
)
# The counters the instrumented loops call; the watchdog thread and the
# snapshot (API only) may allocate and log.
PERF_HOT_SOURCES = (
    PROJECT_ROOT / "src" / "perf" / "mod.rs",
    # Fork (stall attribution): the step marks the server loop sets.
    PROJECT_ROOT / "src" / "perf" / "step.rs",
)
PERF_HOT_FUNCTIONS = (
    "now_ns",
    "inc",
    "add",
    "record",
    "start",
    "record_ns",
    "drop",
    "begin",
    "begin_at",
    "end",
    "record_frame_sent",
    "record_client_frame",
    "client_slot",
    # Fork (stall attribution).
    "step",
    "scheduled",
    "enter",
    "set",
    "set_detail",
    "write",
    "pack_mark",
    "api_method_index",
)
PERF_LOCKS = (
    (re.compile(r"\b(?:Mutex|RwLock|Condvar)\b|\.lock\(\)"), "lock in perf code"),
)
PERF_HOT_WORK = (
    *PERF_LOCKS,
    (
        re.compile(r"\bvec!|\bVec::|\bString\b|\bformat!|\bBox::new|\.to_owned\(|\.to_string\(|\.collect\b"),
        "allocation in a perf hot function",
    ),
    (re.compile(r"\bInstant::now\b|\bSystemTime::now\b"), "clock read outside now_ns"),
    (re.compile(r"\b(?:tracing::)?(?:trace|debug|info|warn|error)!"), "logging in a perf hot function"),
)
PER_BYTE_AND_PER_PANE_SOURCES = (
    *HOT_PATH_SOURCES,
    *sorted((PROJECT_ROOT / "src" / "pty").rglob("*.rs")),
    *sorted((PROJECT_ROOT / "src" / "pane").rglob("*.rs")),
    PROJECT_ROOT / "src" / "pane.rs",
)
PERF_CALL = re.compile(r"\bperf::[A-Za-z_:]+(?:\.[a-z_]+\(\))?")
ALLOWED_PER_BYTE_PERF_CALLS = {
    "perf::server::SYNC_HOLDS.inc()",
    "perf::server::SYNC_HOLD_TIMEOUTS.inc()",
}


def function_bodies(code: str, names) -> list[tuple[int, str]]:
    bodies: list[tuple[int, str]] = []
    for name in names:
        for match in re.finditer(r"\bfn\s+" + re.escape(name) + r"\b[^{;]*\{", code):
            depth = 0
            end = match.end() - 1
            while end < len(code):
                if code[end] == "{":
                    depth += 1
                elif code[end] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                end += 1
            # now_ns is the one sanctioned clock read.
            body = code[match.end() : end]
            if name == "now_ns":
                body = body.replace("Instant::now", "")
            bodies.append((code.count("\n", 0, match.end()) + 1, body))
    return bodies


def find_perf_hot_violations(paths) -> list[str]:
    violations: list[str] = []
    for path in paths:
        code = production_code(path.read_text(encoding="utf-8"))
        for line, body in function_bodies(code, PERF_HOT_FUNCTIONS):
            for pattern, description in PERF_HOT_WORK:
                for match in pattern.finditer(body):
                    offset = body.count("\n", 0, match.start())
                    relative_path = path.relative_to(PROJECT_ROOT)
                    violations.append(f"{relative_path}:{line + offset}: {description}")
    return violations


def find_per_byte_perf_calls(paths) -> list[str]:
    violations: list[str] = []
    for path in paths:
        code = production_code(path.read_text(encoding="utf-8"))
        for match in PERF_CALL.finditer(code):
            if match.group(0) in ALLOWED_PER_BYTE_PERF_CALLS:
                continue
            line = code.count("\n", 0, match.start()) + 1
            relative_path = path.relative_to(PROJECT_ROOT)
            violations.append(
                f"{relative_path}:{line}: perf call {match.group(0)} in per-byte/per-pane code"
            )
    return violations


def perf_heartbeat_order_violations(server: str, client: str) -> list[str]:
    violations: list[str] = []
    select = server.find("tokio::select!")
    after = server.find("};", select)
    following = server[after + 2 :].lstrip().splitlines()[0] if select != -1 and after != -1 else ""
    if "perf::server::LOOP.begin()" not in following:
        violations.append("server: LOOP.begin() must directly follow the event wait")
    for call in ("self.complete_shutdown().await", "self.perform_live_handoff("):
        index = server.find(call)
        # The call's own line and the one above it.
        lines = server[:index].splitlines()[-2:] if index != -1 else []
        if not any("perf::server::LOOP.end()" in line for line in lines):
            violations.append(f"server: LOOP.end() must directly precede {call}")
    last_select = client.rfind("tokio::select!")
    popup = client.find("tick_popup_pending", last_select)
    if last_select == -1 or "perf::client::LOOP.begin()" not in client[last_select:popup]:
        violations.append("client: LOOP.begin() must follow the event wait, before tick_popup_pending")
    return violations


# Fork (stall attribution): each branch of the server loop marks its step,
# so a stall names it. (call in the loop body, the step that must be the
# nearest mark before it)
RUN_LOOP_STEPS = (
    ("self.app.reap_finished_detached_processes()", "LoopTop"),
    ("self.initiate_shutdown()", "Shutdown"),
    ("self.drain_internal_events_with_forwarding_up_to(", "Shutdown"),
    ("self.drain_internal_events_with_forwarding()", "InternalEvents"),
    ("self.app.expire_due_metadata(", "MetadataExpiry"),
    ("self.drain_api_requests_with_shutdown_check()", "ApiRequests"),
    ("self.app.sync_focus_events()", "FocusSync"),
    ("self.app.sync_session_save_schedule()", "FocusSync"),
    ("self.accept_client_connections()", "ClientAccept"),
    ("self.drain_server_events()", "ServerEvents"),
    ("self.handle_scheduled_tasks_headless(", "Scheduled"),
    ("self.poll_pending_alt_screen_reads(", "AltScreenReads"),
    ("self.process_deferred_alt_screen_reads()", "AltScreenReads"),
    ("self.app.ensure_default_workspace()", "DefaultWorkspace"),
    ("self.expire_native_graphics(", "NativeGraphics"),
    ("self.drain_client_config_reload_request()", "ClientSync"),
    ("self.sync_immediate_pty_sources()", "ClientSync"),
    ("self.stream_host_mouse_capture_mode()", "ClientSync"),
    ("self.stream_direct_terminal_keyboard_mode()", "ClientSync"),
    ("self.app.can_render_now(", "RenderPrepare"),
    ("self.sync_terminal_title_sources(", "RenderPrepare"),
    ("self.render_retained_pane_surface_and_stream(", "RenderPrepare"),
    ("self.render_and_stream()", "RenderPrepare"),
    (".next_headless_loop_deadline_with_git_refresh(", "NextDeadline"),
    ("self.handle_internal_event_with_forwarding(", "Dispatch"),
    ("self.handle_api_request_with_shutdown_check(", "Dispatch"),
    ("self.handle_server_event_with_render_impact(", "Dispatch"),
)
# Handlers that can run inside another step: a guard first in their body.
STEP_GUARDED_HANDLERS = (
    ("headless.rs", "handle_server_event", "ServerEvent"),
    ("headless.rs", "handle_api_request_with_shutdown_check_inner", "ApiRequest"),
    ("headless/client_views.rs", "handle_api_request_with_shutdown_check", "ApiRequest"),
    ("headless/client_views.rs", "handle_client_shell_api_request", "ApiRequest"),
    ("headless/notifications.rs", "handle_internal_event_with_forwarding", "AppEvent"),
    ("headless/render.rs", "render_and_stream_with_graphics_limit", "FullRender"),
    ("headless/retained_surface.rs", "render_retained_pane_surface_and_stream", "RetainedRender"),
)
# Scheduled tasks: (call, ScheduledTask that must be the nearest mark).
SCHEDULED_TASK_STEPS = (
    ("self.app.state.toast = None", "Deadlines"),
    ("self.app.state.drain_due_agent_notifications(", "AgentNotifications"),
    ("self.app.start_restored_worktree_validation(", "WorktreeValidation"),
    ("self.app.start_git_status_refresh_if_due(", "GitRefresh"),
    ("self.app.run_auto_update_check()", "UpdateChecks"),
    ("self.app.run_agent_manifest_update_check()", "UpdateChecks"),
    ("self.app.start_background_session_save()", "SessionSave"),
    ("self.app.sync_agent_transcript_backups()", "TranscriptBackup"),
    ("self.app.handle_codex_session_probe(", "AgentProbes"),
    ("self.app.handle_agent_handoff_tasks(", "AgentProbes"),
    ("self.app.expire_metadata_at(", "MetadataExpiry"),
    ("self.app.handle_coordinator_tasks(", "Notices"),
    ("self.flush_news_notifications(", "Notices"),
    ("self.app.drive_pending_agent_closes(", "AgentLifecycle"),
    ("self.app.start_pending_agent_restarts(", "AgentLifecycle"),
    ("self.handle_agent_autosuspend(", "Autosuspend"),
    (".start_pending_agent_resumes(", "AgentResumes"),
)
STEP_MARK = re.compile(r"perf::server::step\(crate::perf::step::Step::(\w+)\)")
SCHEDULED_MARK = re.compile(r"perf::server::scheduled\(crate::perf::step::ScheduledTask::(\w+)\)")


def function_body(code: str, name: str) -> str:
    bodies = function_bodies(code, (name,))
    return bodies[0][1] if bodies else ""


def nearest_mark_violations(body: str, calls, mark: re.Pattern, label: str) -> list[str]:
    violations: list[str] = []
    for call, expected in calls:
        index = body.find(call)
        if index == -1:
            violations.append(f"{label}: {call} not found")
            continue
        while index != -1:
            marks = list(mark.finditer(body, 0, index))
            found = marks[-1].group(1) if marks else None
            if found != expected:
                violations.append(f"{label}: {call} runs under {found}, expected {expected}")
            index = body.find(call, index + 1)
    return violations


def stall_step_coverage_violations(sources: dict[str, str]) -> list[str]:
    server = sources["headless.rs"]
    start = server.find("pub async fn run(&mut self)")
    end = server.find("save_session_on_shutdown", start)
    loop_body = server[start:end] if start != -1 and end != -1 else ""
    violations = nearest_mark_violations(loop_body, RUN_LOOP_STEPS, STEP_MARK, "run loop")
    select = loop_body.find("tokio::select!")
    after = loop_body.find("};", select)
    following = loop_body[after + 2 :].lstrip().splitlines()[:2] if select != -1 and after != -1 else []
    if len(following) < 2 or "Step::Dispatch" not in following[1]:
        violations.append("run loop: Step::Dispatch must follow LOOP.begin() after the wait")
    for file, handler, step in STEP_GUARDED_HANDLERS:
        body = function_body(sources.get(file, ""), handler)
        first = re.split(r"\breturn\b|\bif\b|\bmatch\b", body, maxsplit=1)[0]
        if not re.search(r"perf::server::enter\(\s*crate::perf::step::Step::" + step + r",", first):
            violations.append(f"{file}: {handler} must enter Step::{step} before its first branch")
    scheduled = function_body(server, "handle_scheduled_tasks_headless")
    violations += nearest_mark_violations(scheduled, SCHEDULED_TASK_STEPS, SCHEDULED_MARK, "scheduled")
    return violations


def stall_step_sources() -> dict[str, str]:
    root = PROJECT_ROOT / "src" / "server"
    files = {file for file, _, _ in STEP_GUARDED_HANDLERS} | {"headless.rs"}
    return {file: production_code((root / file).read_text(encoding="utf-8")) for file in files}


def blank_non_newlines(chars: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if chars[index] != "\n":
            chars[index] = " "


def mask_comments_and_literals(source: str) -> str:
    chars = list(source)
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end == -1 else end
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth > 0:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source[index] == "r":
            quote = index + 1
            while quote < len(source) and source[quote] == "#":
                quote += 1
            if quote < len(source) and source[quote] == '"':
                suffix = '"' + "#" * (quote - index - 1)
                end = source.find(suffix, quote + 1)
                end = len(source) if end == -1 else end + len(suffix)
                blank_non_newlines(chars, index, end)
                index = end
                continue

        if source[index] == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            blank_non_newlines(chars, index, min(end, len(source)))
            index = end
            continue

        if source[index] == "'":
            end = index + 2
            if index + 1 < len(source) and source[index + 1] == "\\":
                end += 1
            if end < len(source) and source[end] == "'":
                end += 1
                blank_non_newlines(chars, index, end)
                index = end
                continue

        index += 1

    return "".join(chars)


def production_code(source: str) -> str:
    code = mask_comments_and_literals(source)
    chars = list(code)
    search_from = 0

    while test_module := TEST_MODULE.search(code, search_from):
        depth = 0
        end = test_module.end() - 1
        while end < len(code):
            if code[end] == "{":
                depth += 1
            elif code[end] == "}":
                depth -= 1
                if depth == 0:
                    end += 1
                    break
            end += 1
        blank_non_newlines(chars, test_module.start(), end)
        code = "".join(chars)
        search_from = end

    return code


def find_violations(paths, rules) -> list[str]:
    violations: list[str] = []
    for path in paths:
        code = production_code(path.read_text(encoding="utf-8"))
        for pattern, description in rules:
            for match in pattern.finditer(code):
                line = code.count("\n", 0, match.start()) + 1
                relative_path = path.relative_to(PROJECT_ROOT)
                violations.append(f"{relative_path}:{line}: {description}")
    return violations


class UiHotPathArchitectureTests(unittest.TestCase):
    def test_render_hot_paths_avoid_known_expensive_runtime_queries(self) -> None:
        violations = find_violations(HOT_PATH_SOURCES, FORBIDDEN_CALLS)

        self.assertEqual(
            violations,
            [],
            "Render/layout code must not perform pane-scaled expensive reads:\n"
            + "\n".join(violations),
        )

    def test_app_and_server_avoid_aggregate_terminal_state(self) -> None:
        self.assertTrue(APP_SERVER_SOURCES, "No app/server Rust sources were discovered")
        violations = find_violations(APP_SERVER_SOURCES, AGGREGATE_STATE_CALLS)

        self.assertEqual(
            violations,
            [],
            "App/server code must use narrow terminal-state accessors:\n"
            + "\n".join(violations),
        )

    def test_scanner_ignores_non_production_references(self) -> None:
        source = '''
// runtime.input_state()
const EXAMPLE: &str = "runtime.input_state()";
#[cfg(test)]
mod tests {
    fn aggregate_state_test() { runtime.input_state(); }
}
fn production_after_tests() {}
'''
        code = production_code(source)
        self.assertNotRegex(code, FORBIDDEN_CALLS[0][0])
        self.assertIn("fn production_after_tests()", code)
        self.assertEqual(code.count("\n"), source.count("\n"))

    def test_scanner_checks_production_after_test_modules(self) -> None:
        source = '''
#[cfg(test)]
mod tests {
    const BRACES: &str = "}}";
}
fn render() { TerminalRuntime::input_state; }
'''
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[0][0])

    def test_scanner_catches_each_aggregate_state_call(self) -> None:
        cases = (
            ("fn render() { runtime.input_state(); }", INPUT_STATE_CALL),
            ("fn render() { runtime.keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
            ("fn render() { runtime.kitty_keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
        )
        for source, pattern in cases:
            with self.subTest(source=source):
                self.assertRegex(production_code(source), pattern)

    def test_scanner_catches_imported_process_query(self) -> None:
        source = "fn render() { foreground_job(pid); }"
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[3][0])

    def test_perf_code_never_locks(self) -> None:
        self.assertTrue(PERF_SOURCES, "No perf Rust sources were discovered")
        violations = find_violations(PERF_SOURCES, PERF_LOCKS)
        self.assertEqual(violations, [], "\n".join(violations))

    def test_perf_hot_functions_neither_lock_nor_allocate(self) -> None:
        violations = find_perf_hot_violations(PERF_HOT_SOURCES)
        self.assertEqual(
            violations,
            [],
            "Perf counters run per frame or loop iteration:\n" + "\n".join(violations),
        )

    def test_per_byte_and_per_pane_code_calls_only_per_hold_perf_counters(self) -> None:
        violations = find_per_byte_perf_calls(PER_BYTE_AND_PER_PANE_SOURCES)
        self.assertEqual(violations, [], "\n".join(violations))

    def test_perf_scanners_catch_synthetic_violations(self) -> None:
        source = """
pub(crate) fn record_frame_sent(client_id: u64, bytes: usize) {
    let label = format!("{client_id}");
    let guard = SLOTS.lock();
    let now = Instant::now();
}
pub(crate) fn snapshot() -> Vec<u64> { vec![1] }
"""
        bodies = function_bodies(production_code(source), PERF_HOT_FUNCTIONS)
        self.assertEqual(len(bodies), 1)
        found = {
            description
            for pattern, description in PERF_HOT_WORK
            if pattern.search(bodies[0][1])
        }
        self.assertEqual(
            found,
            {
                "lock in perf code",
                "allocation in a perf hot function",
                "clock read outside now_ns",
            },
        )
        call = "fn feed(byte: u8) { crate::perf::server::FRAMES_SENT.inc(); }"
        allowed = "fn hold() { crate::perf::server::SYNC_HOLDS.inc(); }"
        self.assertTrue(
            [m.group(0) for m in PERF_CALL.finditer(production_code(call))]
            and all(
                m.group(0) not in ALLOWED_PER_BYTE_PERF_CALLS
                for m in PERF_CALL.finditer(production_code(call))
            )
        )
        self.assertEqual(
            [m.group(0) for m in PERF_CALL.finditer(production_code(allowed))],
            ["perf::server::SYNC_HOLDS.inc()"],
        )

    def test_loop_heartbeats_mark_dispatch_busy_and_skip_shutdown_and_handoff(self) -> None:
        # Fork (perf watch): each loop is busy from the moment its wait returns
        # (dispatching the event counts), and the server's shutdown and live
        # handoff are not watched as a stall.
        server = production_code((PROJECT_ROOT / "src" / "server" / "headless.rs").read_text(encoding="utf-8"))
        client = production_code((PROJECT_ROOT / "src" / "client" / "mod.rs").read_text(encoding="utf-8"))
        self.assertEqual(perf_heartbeat_order_violations(server, client), [])

    def test_server_loop_branches_mark_their_stall_step(self) -> None:
        # Fork (stall attribution): every branch of the server loop, every
        # handler that can nest and every scheduled task marks its step.
        self.assertEqual(stall_step_coverage_violations(stall_step_sources()), [])

    def test_stall_step_scanner_catches_a_missing_mark(self) -> None:
        sources = stall_step_sources()
        server = sources["headless.rs"]
        # Drop the ServerEvents mark: the drain then runs under ClientAccept.
        sources["headless.rs"] = server.replace(
            "crate::perf::server::step(crate::perf::step::Step::ServerEvents);", "", 1
        )
        self.assertEqual(
            stall_step_coverage_violations(sources),
            ["run loop: self.drain_server_events() runs under ClientAccept, expected ServerEvents"],
        )
        # Drop the session-save mark and a handler guard.
        sources["headless.rs"] = server.replace(
            "crate::perf::server::scheduled(crate::perf::step::ScheduledTask::SessionSave);", "", 1
        )
        sources["headless/render.rs"] = sources["headless/render.rs"].replace(
            "crate::perf::server::enter(", "crate::perf::server::skipped(", 1
        )
        self.assertEqual(
            stall_step_coverage_violations(sources),
            [
                "headless/render.rs: render_and_stream_with_graphics_limit must enter Step::FullRender before its first branch",
                "scheduled: self.app.start_background_session_save() runs under UpdateChecks, expected SessionSave",
            ],
        )

    def test_loop_heartbeat_order_scanner_catches_a_missing_mark(self) -> None:
        server = """
loop {
    crate::perf::server::LOOP.begin();
    if self.shutting_down {
        self.complete_shutdown().await?;
    }
    crate::perf::server::LOOP.end();
    let event = {
        tokio::select! {
            _ = a => 1,
        }
    };
    handle(event);
}
let handoff_result = self.perform_live_handoff(params.clone());
"""
        client = """
let event = tokio::select! { _ = a => 1, };
let now = Instant::now();
shell.tick_popup_pending(now);
"""
        self.assertEqual(
            len(perf_heartbeat_order_violations(server, client)),
            4,
        )


if __name__ == "__main__":
    unittest.main()
