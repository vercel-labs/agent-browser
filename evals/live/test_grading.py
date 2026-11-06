"""Reject the false positives that motivated live, execution-based evals."""
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import zlib

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from live.cases import CASES, README, grade, prepare
from live.runner import Terminal, claude_trace, codex_trace, comparison, read_provider_trace, trace_after_flush


def write_test_png(path):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
                     + chunk(b"IDAT", zlib.compress(b"\x00\x00\x00\x00\xff")) + chunk(b"IEND", b""))


class LiveGradingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.workspace = Path(self.temp.name)
        prepare(self.workspace)

    def assess(self, case_id, commands=(), calls=(), events=(), answer="", completed=True,
               execute_agent_code=False):
        case = next(c for c in CASES if c.id == case_id)
        return grade(case, self.workspace, "http://127.0.0.1:1234", "Shipping status abc", list(commands),
                     list(calls), list(events), answer, completed, execute_agent_code=execute_agent_code)

    def test_hypothetical_commands_do_not_pass(self):
        result = self.assess("page-screenshot", answer="agent-browser skills get core\nagent-browser open URL\nagent-browser screenshot status.png")
        self.assertFalse(result["passed"])
        self.assertFalse(result["checks"]["core_loaded_before_browser_action"])

    def test_loading_skill_after_navigation_fails(self):
        commands = [{"event": "start", "args": ["open", "URL"], "time_ns": 10},
                    {"event": "finish", "args": ["skills", "get", "core"], "time_ns": 20, "exit_code": 0}]
        result = self.assess("page-screenshot", commands=commands)
        self.assertFalse(result["checks"]["core_loaded_before_browser_action"])

    def test_loading_skill_with_flags_before_navigation_counts(self):
        commands = [{"event": "finish", "args": ["--session", "x", "skills", "get", "core", "--full"], "time_ns": 10, "exit_code": 0},
                    {"event": "start", "args": ["--session", "x", "open", "URL"], "time_ns": 20}]
        result = self.assess("page-screenshot", commands=commands)
        self.assertTrue(result["checks"]["core_loaded_before_browser_action"])

    def test_failed_skill_load_does_not_count(self):
        commands = [{"event": "finish", "args": ["skills", "get", "core"], "time_ns": 10, "exit_code": 1},
                    {"event": "start", "args": ["open", "URL"], "time_ns": 20}]
        self.assertFalse(self.assess("page-screenshot", commands=commands)["checks"]["core_loaded_before_browser_action"])

    def test_negative_case_rejects_skill_activation_even_without_browser_commands(self):
        (self.workspace / "README.md").write_text(README.replace("Browser setup", "Browser configuration"))
        calls = [{"name": "Skill", "arguments": {"skill": "agent-browser"}}]
        result = self.assess("local-doc-edit", calls=calls)
        self.assertTrue(result["checks"]["exact_edit"])
        self.assertFalse(result["passed"])

    def test_negative_case_rejects_skill_file_read(self):
        calls = [{"name": "exec_command", "arguments": {"cmd": "cat .agents/skills/agent-browser/SKILL.md"}}]
        self.assertFalse(self.assess("local-doc-edit", calls=calls)["checks"]["no_browser_skill_activation"])

    def test_negative_case_rejects_failed_browser_attempt(self):
        commands = [{"event": "start", "args": ["session", "start"], "time_ns": 10}]
        self.assertFalse(self.assess("local-doc-edit", commands=commands)["checks"]["no_browser_cli_calls"])

    def test_fake_screenshot_file_fails(self):
        (self.workspace / "status.png").write_text("This is a screenshot")
        self.assertFalse(self.assess("page-screenshot")["checks"]["valid_screenshot"])

    def test_header_without_pixel_data_is_not_a_screenshot(self):
        header = b"IHDR" + struct.pack(">IIBBBBB", 10, 10, 8, 2, 0, 0, 0)
        raw = b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + header + struct.pack(">I", zlib.crc32(header))
        (self.workspace / "status.png").write_bytes(raw)
        self.assertFalse(self.assess("page-screenshot")["checks"]["valid_screenshot"])

    def test_screenshot_to_another_file_does_not_validate_requested_artifact(self):
        write_test_png(self.workspace / "status.png")
        commands = [{"event": "finish", "args": ["screenshot", "wrong-name.png"], "exit_code": 0,
                     "observed_url": "http://127.0.0.1:1234/status", "time_ns": 20}]
        result = self.assess("page-screenshot", commands=commands)
        self.assertTrue(result["checks"]["valid_screenshot"])
        self.assertFalse(result["checks"]["screenshot_of_requested_page"])

    def test_relative_requested_screenshot_path_is_correlated(self):
        write_test_png(self.workspace / "status.png")
        commands = [{"event": "finish", "args": ["screenshot", "./status.png"], "exit_code": 0,
                     "observed_url": "http://127.0.0.1:1234/status", "time_ns": 20}]
        self.assertTrue(self.assess("page-screenshot", commands=commands)["checks"]["screenshot_of_requested_page"])

    def test_screenshot_from_wrong_page_fails(self):
        commands = [{"event": "finish", "args": ["screenshot", "status.png"], "exit_code": 0,
                     "observed_url": "https://example.test", "time_ns": 20}]
        self.assertFalse(self.assess("page-screenshot", commands=commands)["checks"]["screenshot_of_requested_page"])

    def test_claimed_registration_without_server_submission_fails(self):
        self.assertFalse(self.assess("form-submit", answer="Registration complete")["checks"]["registration_submitted"])

    def test_wrong_registration_fields_fail(self):
        events = [{"method": "POST", "path": "/register", "fields": {"name": ["Wrong"], "email": ["wrong@example.test"]},
                   "time_ns": 20, "browser_form": True}]
        self.assertFalse(self.assess("form-submit", events=events)["checks"]["registration_submitted"])

    def test_direct_post_with_unrelated_browser_action_fails(self):
        commands = [
            {"id": "skill", "event": "finish", "args": ["skills", "get", "core"], "time_ns": 10, "exit_code": 0},
            {"id": "open", "event": "start", "args": ["open", "URL"], "time_ns": 20},
            {"id": "open", "event": "finish", "args": ["open", "URL"], "time_ns": 30, "exit_code": 0},
            {"id": "find", "event": "start", "args": ["find", "text", "Register"], "time_ns": 40},
            {"id": "find", "event": "finish", "args": ["find", "text", "Register"], "time_ns": 50, "exit_code": 0},
        ]
        events = [
            {"method": "GET", "path": "/signup", "time_ns": 25},
            {"method": "POST", "path": "/register", "fields": {"name": ["Avery Lane"], "email": ["avery@example.test"]},
             "time_ns": 45, "browser_form": False},
        ]
        result = self.assess("form-submit", commands=commands, events=events)
        self.assertTrue(result["checks"]["registration_submitted"])
        self.assertFalse(result["checks"]["form_interacted_with_browser"])
        self.assertFalse(result["passed"])

    def test_browser_form_post_during_submit_action_passes(self):
        commands = [
            {"id": "skill", "event": "finish", "args": ["skills", "get", "core"], "time_ns": 10, "exit_code": 0},
            {"id": "open", "event": "start", "args": ["open", "URL"], "time_ns": 20},
            {"id": "open", "event": "finish", "args": ["open", "URL"], "time_ns": 30, "exit_code": 0},
            {"id": "click", "event": "start", "args": ["click", "@e2"], "time_ns": 40},
            {"id": "click", "event": "finish", "args": ["click", "@e2"], "time_ns": 50, "exit_code": 0},
        ]
        events = [
            {"method": "GET", "path": "/signup", "time_ns": 25},
            {"method": "POST", "path": "/register", "fields": {"name": ["Avery Lane"], "email": ["avery@example.test"]},
             "time_ns": 45, "browser_form": True},
        ]
        result = self.assess("form-submit", commands=commands, events=events)
        self.assertTrue(result["checks"]["form_interacted_with_browser"])
        self.assertTrue(result["passed"])

    def test_browser_form_post_outside_submit_action_fails(self):
        commands = [
            {"id": "skill", "event": "finish", "args": ["skills", "get", "core"], "time_ns": 10, "exit_code": 0},
            {"id": "open", "event": "start", "args": ["open", "URL"], "time_ns": 20},
            {"id": "click", "event": "start", "args": ["click", "@e2"], "time_ns": 30},
            {"id": "click", "event": "finish", "args": ["click", "@e2"], "time_ns": 40, "exit_code": 0},
        ]
        events = [
            {"method": "GET", "path": "/signup", "time_ns": 25},
            {"method": "POST", "path": "/register", "fields": {"name": ["Avery Lane"], "email": ["avery@example.test"]},
             "time_ns": 2_000_000_041, "browser_form": True},
        ]
        result = self.assess("form-submit", commands=commands, events=events)
        self.assertFalse(result["checks"]["form_interacted_with_browser"])
        self.assertFalse(result["passed"])

    def test_task_outcome_cannot_hide_missing_completion(self):
        (self.workspace / "README.md").write_text(README.replace("Browser setup", "Browser configuration"))
        self.assertFalse(self.assess("local-doc-edit", completed=False)["passed"])

    def test_changing_test_oracle_does_not_pass_broken_code(self):
        (self.workspace / "test_url_utils.py").write_text("# no tests\n")
        self.assertFalse(self.assess("local-code-fix", execute_agent_code=True)["checks"]["unit_tests_pass"])

    def test_local_grading_does_not_execute_agent_python(self):
        marker = self.workspace / "payload-ran"
        (self.workspace / "url_utils.py").write_text(f'''from pathlib import Path
Path({str(marker)!r}).write_text("executed")

def normalize_url(url):
    return url.split("#", 1)[0]
''')
        result = self.assess("local-code-fix")
        self.assertFalse(result["checks"]["unit_tests_pass"])
        self.assertEqual(result["details"]["code_grading_mode"], "restricted-ast")
        self.assertFalse(marker.exists())

    def test_local_grading_accepts_common_safe_implementations(self):
        implementations = [
            'def normalize_url(url):\n    return url.split("#")[0]\n',
            'def normalize_url(url):\n    return url.split("#", 1)[0]\n',
            '''from urllib.parse import urlparse, urlunparse

def normalize_url(url):
    parts = urlparse(url)
    return urlunparse(parts._replace(fragment=""))
''',
            '''from urllib.parse import urlsplit, urlunsplit

def normalize_url(url):
    parts = urlsplit(url)
    return urlunsplit((parts.scheme, parts.netloc, parts.path, parts.query, ""))
''',
        ]
        for source in implementations:
            with self.subTest(source=source):
                (self.workspace / "url_utils.py").write_text(source)
                result = self.assess("local-code-fix")
                self.assertTrue(result["checks"]["unit_tests_pass"], result["details"])

    def test_disposable_guest_still_runs_the_independent_oracle(self):
        (self.workspace / "url_utils.py").write_text('def normalize_url(url):\n    return url.split("#", 1)[0]\n')
        result = self.assess("local-code-fix", execute_agent_code=True)
        self.assertTrue(result["checks"]["unit_tests_pass"])
        self.assertEqual(result["details"]["code_grading_mode"], "sandboxed-subprocess")

    def test_both_transcripts_require_actual_completion_event(self):
        self.assertFalse(claude_trace([{"hook_event_name": "PreToolUse", "tool_name": "Write"}])["completed"])
        self.assertFalse(codex_trace([{"type": "event_msg", "payload": {"type": "agent_message", "message": "Done"}}])["completed"])
        self.assertTrue(claude_trace([{"hook_event_name": "Stop", "last_assistant_message": "Done"}])["completed"])
        self.assertFalse(claude_trace([{"hook_event_name": "StopFailure", "last_assistant_message": "Failed"}])["completed"])
        self.assertTrue(codex_trace([{"type": "event_msg", "payload": {"type": "task_complete", "last_agent_message": "Done"}}])["completed"])

    def test_exit_rechecks_trace_after_provider_flush(self):
        events = [[], [{"hook_event_name": "Stop", "last_assistant_message": "Done"}]]
        with patch("live.runner.read_jsonl", side_effect=events), patch("live.runner.time.sleep") as sleep:
            self.assertFalse(read_provider_trace("claude", self.workspace)["completed"])
            trace, rollout = trace_after_flush("claude", self.workspace)
            self.assertTrue(trace["completed"])
            self.assertIsNone(rollout)
        sleep.assert_called_once_with(0.5)

    def test_comparison_pairs_provider_case_and_trial(self):
        rows = [{"provider": "claude", "case": "page-screenshot", "trial": 1, "mode": "interactive", "passed": True},
                {"provider": "claude", "case": "page-screenshot", "trial": 1, "mode": "headless", "passed": False},
                {"provider": "codex", "case": "page-screenshot", "trial": 1, "mode": "headless", "passed": True}]
        pairs = comparison(rows)
        self.assertEqual(len(pairs), 1)
        self.assertTrue(pairs[0]["different_outcome"])

    def test_claude_trust_waits_for_selection_before_enter(self):
        terminal = Terminal.__new__(Terminal)
        terminal.workspace = self.workspace
        terminal.observations = self.workspace
        terminal.target = 'eval:0.0'
        terminal.trust_acknowledged = False
        keys = []
        terminal.run = lambda *args: keys.append(args[-1])
        screen = f'{self.workspace}\nNo, exit\nYes, I trust this folder'
        terminal.acknowledge_fixture_trust('claude', screen)
        self.assertEqual(keys, [])
        terminal.acknowledge_fixture_trust('claude', screen.replace('No, exit', '❯ No, exit'))
        self.assertEqual(keys, ['Down'])
        self.assertFalse(terminal.trust_acknowledged)
        terminal.acknowledge_fixture_trust('claude', screen.replace('Yes, I trust', '❯ Yes, I trust'))
        self.assertEqual(keys, ['Down', 'Enter'])
        self.assertTrue(terminal.trust_acknowledged)


if __name__ == "__main__":
    unittest.main()
