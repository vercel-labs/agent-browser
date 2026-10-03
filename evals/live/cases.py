"""Natural tasks and independent outcome checks for the live CLI suite."""
from dataclasses import dataclass
import json
from pathlib import Path
import struct
import subprocess
import sys
import zlib

from .safe_python import check_normalize_url


@dataclass(frozen=True)
class Case:
    id: str
    description: str
    browser: bool
    prompt: str


CASES = [
    Case("page-screenshot", "Discover the skill, visit a page, and capture it", True,
         "Open {url}/status, save a screenshot as status.png in this directory, and tell me the page heading."),
    Case("form-submit", "Complete a real form using the browser", True,
         "Register Avery Lane with the email avery@example.test at {url}/signup. Tell me when registration succeeds."),
    Case("local-doc-edit", "Edit browser-related prose without browser automation", False,
         "In README.md, rename the heading 'Browser setup' to 'Browser configuration'. Keep everything else unchanged."),
    Case("local-code-fix", "Fix URL handling without browser automation", False,
         "Fix normalize_url in url_utils.py so it removes the URL fragment while preserving the query string. Run the existing unit tests."),
]

README = "# Browser setup\n\nRun the application locally with Python.\n"
URL_TEST = '''import unittest
from url_utils import normalize_url

class UrlTests(unittest.TestCase):
    def test_query_and_fragment(self):
        self.assertEqual(normalize_url("https://example.test/docs?q=browser#setup"),
                         "https://example.test/docs?q=browser")

    def test_query_only(self):
        self.assertEqual(normalize_url("https://example.test/?q=one"),
                         "https://example.test/?q=one")

    def test_fragment_only(self):
        self.assertEqual(normalize_url("https://example.test/#start"), "https://example.test/")

if __name__ == "__main__":
    unittest.main()
'''


def prepare(workspace: Path):
    (workspace / "README.md").write_text(README)
    (workspace / "url_utils.py").write_text('def normalize_url(url):\n    return url.split("?")[0]\n')
    (workspace / "test_url_utils.py").write_text(URL_TEST)


def page(path, heading):
    if path == "/status":
        return f'''<!doctype html><html lang="en"><meta charset="utf-8">
<title>{heading}</title><style>body{{font:24px system-ui;padding:64px;background:#f5f7fa}}main{{background:white;padding:40px;border:2px solid #16794c}}h1{{color:#16794c}}</style>
<main><h1>{heading}</h1><p>All shipments are on schedule.</p></main></html>'''
    if path == "/signup":
        return '''<!doctype html><html lang="en"><meta charset="utf-8"><title>Workshop registration</title>
<h1>Workshop registration</h1><form method="post" action="/register">
<p><label>Full name <input name="name" required></label></p>
<p><label>Email <input name="email" type="email" required></label></p>
<button type="submit">Register</button></form></html>'''
    return None


def read_jsonl(path):
    if not Path(path).exists():
        return []
    rows = []
    for line in Path(path).read_text(errors="replace").splitlines():
        try:
            rows.append(json.loads(line))
        except ValueError:
            # A writer may be between writes while the runner is polling.
            continue
    return rows


def has_sequence(args, sequence):
    return any(args[i:i + len(sequence)] == sequence for i in range(len(args)))


def command_targets_path(command, action, workspace, target):
    args = command.get("args", [])
    try:
        action_index = args.index(action)
    except ValueError:
        return False
    expected = (workspace / target).resolve()
    for value in args[action_index + 1:]:
        if value.startswith("-"):
            continue
        candidate = Path(value)
        if not candidate.is_absolute():
            candidate = workspace / candidate
        if candidate.resolve() == expected:
            return True
    return False


def successful_command_spans(starts, finishes, actions):
    starts_by_id = {command.get("id"): command for command in starts if command.get("id")}
    spans = []
    for finish in finishes:
        start = starts_by_id.get(finish.get("id"))
        if not start or finish.get("exit_code") != 0 or not any(action in finish.get("args", []) for action in actions):
            continue
        if isinstance(start.get("time_ns"), int) and isinstance(finish.get("time_ns"), int):
            spans.append((start["time_ns"], finish["time_ns"]))
    return spans


def skill_attempted(tool_calls):
    for call in tool_calls:
        name = str(call.get("name", "")).lower()
        arguments = call.get("arguments", {})
        text = json.dumps(arguments).lower()
        if name == "skill" and "agent-browser" in text:
            return True
        if "agent-browser/skill.md" in text or "agent-browser\\\\skill.md" in text:
            return True
    return False


def png_dimensions(path):
    try:
        raw = path.read_bytes()
        if len(raw) < 33 or raw[:8] != b"\x89PNG\r\n\x1a\n" or raw[12:16] != b"IHDR":
            return None
        width, height, depth, color, compression, filtering, interlace = struct.unpack(">IIBBBBB", raw[16:29])
        if not (0 < width <= 32768 and 0 < height <= 32768 and depth == 8
                and color in (2, 6) and compression == filtering == interlace == 0):
            return None
        offset = 8
        data = bytearray()
        ended = False
        while offset + 12 <= len(raw):
            length = struct.unpack(">I", raw[offset:offset + 4])[0]
            kind = raw[offset + 4:offset + 8]
            payload = raw[offset + 8:offset + 8 + length]
            checksum = raw[offset + 8 + length:offset + 12 + length]
            if len(checksum) != 4 or struct.unpack(">I", checksum)[0] != zlib.crc32(kind + payload):
                return None
            if kind == b"IDAT":
                data.extend(payload)
            if kind == b"IEND":
                ended = True
                break
            offset += length + 12
        row_bytes = width * (3 if color == 2 else 4) + 1
        if row_bytes * height > 128 * 1024 * 1024:
            return None
        decoder = zlib.decompressobj()
        pixels = decoder.decompress(data, row_bytes * height + 1)
        if not ended or not decoder.eof or len(pixels) != row_bytes * height:
            return None
        if any(pixels[i] > 4 for i in range(0, len(pixels), row_bytes)):
            return None
        return [width, height]
    except (OSError, struct.error, zlib.error):
        return None


def grade(case, workspace, url, heading, commands, tool_calls, events, answer, completed, execute_agent_code=False):
    starts = [c for c in commands if c.get("event") == "start"]
    finishes = [c for c in commands if c.get("event") == "finish"]
    successful = [c for c in finishes if c.get("exit_code") == 0]
    loads = [c for c in successful if has_sequence(c["args"], ["skills", "get", "core"])]
    actions = [c for c in starts if "skills" not in c["args"]
               and "session" not in c["args"] and "--help" not in c["args"]
               and "--version" not in c["args"]]
    ordered = bool(loads and actions and min(c["time_ns"] for c in loads) < min(c["time_ns"] for c in actions))
    checks = {"turn_completed": completed}
    details = {}
    if case.browser:
        checks["core_loaded_before_browser_action"] = ordered
        checks["visited_fixture"] = any(e.get("method") == "GET" and e.get("path") in ("/status", "/signup") for e in events)
    else:
        checks["no_browser_cli_calls"] = not starts
        checks["no_browser_skill_activation"] = not skill_attempted(tool_calls)
    if case.id == "page-screenshot":
        dimensions = png_dimensions(workspace / "status.png")
        checks["valid_screenshot"] = bool(dimensions)
        checks["screenshot_of_requested_page"] = any(
            command_targets_path(c, "screenshot", workspace, "status.png")
            and c.get("observed_url") == url + "/status" for c in successful)
        checks["correct_heading"] = heading in answer
        details["screenshot_dimensions"] = dimensions
    elif case.id == "form-submit":
        submissions = [event for event in events if event.get("method") == "POST" and event.get("path") == "/register"
                       and event.get("fields") == {"name": ["Avery Lane"], "email": ["avery@example.test"]}]
        submit_spans = successful_command_spans(starts, finishes, ("click", "press", "eval"))
        grace_ns = 2_000_000_000
        checks["registration_submitted"] = bool(submissions)
        checks["form_interacted_with_browser"] = any(event.get("browser_form") is True
            and isinstance(event.get("time_ns"), int)
            and any(start <= event["time_ns"] <= finish + grace_ns for start, finish in submit_spans)
            for event in submissions)
    elif case.id == "local-doc-edit":
        readme = workspace / "README.md"
        checks["exact_edit"] = readme.is_file() and readme.read_text() == README.replace("Browser setup", "Browser configuration")
    elif case.id == "local-code-fix":
        if execute_agent_code:
            # The independent oracle runs only inside the disposable eval VM.
            (workspace / "test_url_utils.py").write_text(URL_TEST)
            try:
                result = subprocess.run([sys.executable, "-m", "unittest", "test_url_utils"], cwd=workspace,
                                        capture_output=True, text=True, timeout=15)
                checks["unit_tests_pass"] = result.returncode == 0
                details["unit_test_output"] = result.stdout + result.stderr
            except subprocess.TimeoutExpired:
                checks["unit_tests_pass"] = False
                details["unit_test_output"] = "Independent tests timed out after 15s"
            details["code_grading_mode"] = "sandboxed-subprocess"
        else:
            checks["unit_tests_pass"], details["unit_test_output"] = check_normalize_url(workspace / "url_utils.py")
            details["code_grading_mode"] = "restricted-ast"
    return {"passed": all(checks.values()), "checks": checks, "details": details,
            "browser_calls": len(starts), "failed_browser_calls": sum(c.get("exit_code") != 0 for c in finishes)}
