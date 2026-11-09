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
PERF_HOT_SOURCES = (PROJECT_ROOT / "src" / "perf" / "mod.rs",)
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


if __name__ == "__main__":
    unittest.main()
