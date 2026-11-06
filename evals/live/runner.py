"""Compare actual interactive TUIs with non-interactive CLI sessions."""
import argparse
from datetime import datetime
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlsplit
import uuid

from .cases import CASES, grade, page, prepare, read_jsonl


ROOT = Path(__file__).resolve().parents[2]
CAPTURE = Path(__file__).with_name("capture.py")


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def executable(value):
    path = shutil.which(str(value))
    if not path:
        raise ValueError(f"Executable not found: {value}")
    return str(Path(path).absolute())


def version(command):
    result = subprocess.run([command, "--version"], capture_output=True, text=True, timeout=15)
    return result.stdout.strip() or result.stderr.strip()


class Fixture:
    """Keep expected page content and submitted state outside the agent workspace."""

    def __init__(self):
        self.heading = "Shipping status " + uuid.uuid4().hex[:8]
        self.events = []
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                path = urlsplit(self.path).path
                content = page(path, owner.heading)
                owner.events.append({"method": "GET", "path": path, "time_ns": time.time_ns()})
                self.send_response(200 if content else 404)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.end_headers()
                self.wfile.write((content or "Not found").encode())

            def do_POST(self):
                path = urlsplit(self.path).path
                length = int(self.headers.get("Content-Length", "0"))
                fields = parse_qs(self.rfile.read(min(length, 16384)).decode())
                user_agent = self.headers.get("User-Agent", "")
                referer = urlsplit(self.headers.get("Referer", ""))
                owner.events.append({"method": "POST", "path": path, "fields": fields,
                                     "time_ns": time.time_ns(),
                                     "browser_form": self.headers.get("Origin") == owner.url
                                     and referer.path == "/signup" and "Chrome/" in user_agent})
                self.send_response(200 if path == "/register" else 404)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.end_headers()
                self.wfile.write(b"<!doctype html><title>Registration complete</title><h1>Registration complete</h1>")

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def claude_trace(events):
    calls = [{"name": row.get("tool_name"), "arguments": row.get("tool_input", {})}
             for row in events if row.get("hook_event_name") == "PreToolUse"]
    stops = [row for row in events if row.get("hook_event_name") in ("Stop", "StopFailure")]
    starts = [row for row in events if row.get("hook_event_name") == "SessionStart"]
    return {"completed": bool(stops) and stops[-1].get("hook_event_name") == "Stop",
            "answer": stops[-1].get("last_assistant_message", "") if stops else "",
            "tool_calls": calls, "model": next((r.get("model") for r in starts if r.get("model")), None),
            "transcript_path": next((r.get("transcript_path") for r in reversed(events) if r.get("transcript_path")), None)}


def codex_trace(rows):
    """Read only the eval's rollout; fail closed if completion events disappear."""
    calls = []
    answer = ""
    completed = False
    model = None
    for row in rows:
        payload = row.get("payload", {})
        if row.get("type") == "turn_context":
            model = payload.get("model", model)
        if row.get("type") == "response_item":
            if payload.get("type") in ("function_call", "custom_tool_call"):
                arguments = payload.get("arguments", payload.get("input", ""))
                try:
                    arguments = json.loads(arguments)
                except (ValueError, TypeError):
                    pass
                calls.append({"name": payload.get("name"), "arguments": arguments})
        if row.get("type") == "event_msg" and payload.get("type") in ("task_complete", "turn_complete"):
            completed = True
            answer = payload.get("last_agent_message", payload.get("last_assistant_message", "")) or ""
        if row.get("type") == "event_msg" and payload.get("type") == "agent_message":
            answer = payload.get("message", answer)
    return {"completed": completed, "answer": answer, "tool_calls": calls, "model": model}


def read_provider_trace(provider, observations, rollout=None):
    if provider == "claude":
        return claude_trace(read_jsonl(observations / "hooks.jsonl"))
    if rollout:
        return codex_trace(read_jsonl(rollout))
    return {"completed": False, "answer": "", "tool_calls": [], "model": None}


def trace_after_flush(provider, observations, rollout=None, discover_rollout=None):
    """Wait for provider persistence, then read the final trace once more."""
    time.sleep(0.5)
    if provider == "codex" and rollout is None and discover_rollout:
        rollout = discover_rollout()
    return read_provider_trace(provider, observations, rollout), rollout


def skill_sources(rows, tool_calls):
    sources = set()
    for row in rows:
        content = row.get("message", {}).get("content", "")
        if isinstance(content, list):
            content = "\n".join(str(item.get("text", item.get("content", ""))) for item in content if isinstance(item, dict))
        for source in re.findall(r"Base directory for this skill: ([^\n]+)", str(content)):
            if source.endswith("/agent-browser"):
                sources.add(source + "/SKILL.md")
    for call in tool_calls:
        for source in re.findall(r"/[^\s\"'`]+/agent-browser/SKILL\.md", str(call.get("arguments", ""))):
            sources.add(source)
    return sorted(sources)


def find_rollout(workspace, started, codex_root):
    # Checking metadata first avoids reading unrelated conversations on this host.
    candidates = []
    for path in (codex_root / "sessions").rglob("*.jsonl"):
        try:
            if path.stat().st_mtime < started:
                continue
            with path.open() as stream:
                metadata = json.loads(stream.readline())
            if metadata.get("type") == "session_meta" and metadata.get("payload", {}).get("cwd") == str(workspace):
                candidates.append(path)
        except (OSError, ValueError):
            continue
    if len(candidates) > 1:
        raise RuntimeError("Multiple Codex sessions matched this isolated workspace")
    return candidates[0] if candidates else None


def provider_command(args, provider, mode, workspace, observations, prompt, env, browser_env):
    if provider == "claude":
        hook = shlex.join([sys.executable, str(CAPTURE), str(observations / "hooks.jsonl")])
        settings = {"hooks": {event: [{"hooks": [{"type": "command", "command": hook, "timeout": 10}]}]
                    for event in ("SessionStart", "PreToolUse", "PostToolUse", "Stop", "StopFailure")}}
        settings_path = observations / "claude-settings.json"
        write_json(settings_path, settings)
        command = [args.claude, "--session-id", str(uuid.uuid4()), "--settings", str(settings_path)]
        if args.sandbox_unattended:
            command.append("--dangerously-skip-permissions")
        if args.claude_model:
            command.extend(["--model", args.claude_model])
        if args.claude_permission_mode:
            command.extend(["--permission-mode", args.claude_permission_mode])
        if mode == "headless":
            command.extend(["-p", "--output-format", "stream-json", "--verbose"])
        return [*command, prompt]
    command = [args.codex]
    if mode == "headless":
        command.extend(["exec", "--json"])
    else:
        command.append("--no-alt-screen")
    command.extend(["-C", str(workspace)])
    if args.sandbox_unattended:
        command.append("--dangerously-bypass-approvals-and-sandbox")
    # Codex's shared shell host can restore a pre-existing shell snapshot. Set
    # the fixture environment through its supported per-session policy as well.
    for key, value in {"PATH": env["PATH"], **browser_env}.items():
        command.extend(["-c", f"shell_environment_policy.set.{key}={json.dumps(value)}"])
    if args.codex_model:
        command.extend(["--model", args.codex_model])
    if args.codex_auto_approve:
        command.append("--approve-for-me")
    return [*command, prompt]


def setup_workspace(args, provider, workspace, observations):
    prepare(workspace)
    skill_dir = workspace / (".claude" if provider == "claude" else ".agents") / "skills/agent-browser"
    skill_dir.mkdir(parents=True)
    shutil.copyfile(args.skill, skill_dir / "SKILL.md")
    runtime_skills = workspace / ".agent-browser/skill-data"
    shutil.copytree(args.skills_dir, runtime_skills)
    bin_dir = workspace / "bin"
    bin_dir.mkdir()
    shutil.copyfile(Path(__file__).with_name("browser.py"), bin_dir / "agent-browser")
    shutil.copyfile(CAPTURE, bin_dir / "capture.py")
    (bin_dir / "agent-browser").chmod(0o755)
    browser_env = {"AGENT_BROWSER_SOCKET_DIR": str(workspace / "sockets"),
                   "AGENT_BROWSER_SESSION": "live-eval", "AGENT_BROWSER_SKILLS_DIR": str(runtime_skills),
                   "AGENT_BROWSER_HEADED": "1" if args.browser_mode == "headed" else "0"}
    if os.environ.get("DISPLAY"):
        browser_env["DISPLAY"] = os.environ["DISPLAY"]
    if args.chrome:
        browser_env["AGENT_BROWSER_EXECUTABLE_PATH"] = args.chrome
    write_json(bin_dir / "browser.json", {"binary": args.binary, "env": browser_env,
                                         "log": str(observations / "commands.jsonl")})
    # Git establishes a normal project root without adding agent instructions.
    subprocess.run(["git", "init", "--quiet", str(workspace)], check=True)
    env = os.environ.copy()
    for key in ("CODEX_THREAD_ID", "CLAUDECODE", "TMUX", "AGENT_BROWSER_CDP", "AGENT_BROWSER_AUTO_CONNECT",
                "AGENT_BROWSER_PROFILE", "AGENT_BROWSER_NAMESPACE", "AGENT_BROWSER_CONFIG", "AGENT_BROWSER_PROVIDER"):
        env.pop(key, None)
    env.update(browser_env)
    env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")
    return env, browser_env


class Terminal:
    """A real TUI in an attachable tmux PTY, including its permission dialogs."""

    def __init__(self, args, name, workspace, observations, command, env):
        self.socket = str(workspace / "tmux.sock")
        self.target = "eval:0.0"
        self.args = [args.tmux, "-S", self.socket]
        self.observations = observations
        self.workspace = workspace
        self.trust_acknowledged = False
        launcher = observations / "launch.py"
        config = observations / "launch.json"
        # Credentials remain in the inherited process environment, never this manifest.
        write_json(config, {"argv": command, "cwd": str(workspace)})
        launcher.write_text('''import json, os, pathlib, subprocess, time
folder = pathlib.Path(__file__).parent
config = json.loads((folder / "launch.json").read_text())
while not (folder / "ready").exists():
    time.sleep(0.05)
(folder / "pty.json").write_text(json.dumps({"stdin_is_tty": os.isatty(0), "stdout_is_tty": os.isatty(1)}))
result = subprocess.run(config["argv"], cwd=config["cwd"])
(folder / "exit.json").write_text(json.dumps({"exit_code": result.returncode}))
''')
        subprocess.run([*self.args, "new-session", "-d", "-x", "140", "-y", "45", "-s", "eval", "-n", name,
                        "-c", str(workspace), shlex.join([sys.executable, str(launcher)])], env=env, check=True)
        self.run("set-option", "-g", "remain-on-exit", "on")
        self.run("set-window-option", "-t", "eval:0", "window-size", "manual")
        self.run("resize-window", "-t", "eval:0", "-x", "140", "-y", "60")
        self.run("pipe-pane", "-o", "-t", self.target, "cat > " + shlex.quote(str(observations / "terminal.raw")))
        (observations / "ready").touch()
        attach = shlex.join([*self.args, "attach-session", "-t", "eval"])
        print(f"  Watch: {attach}", flush=True)
        attach_path = observations / "attach.command"
        attach_path.write_text("#!/bin/sh\nexec " + attach + "\n")
        attach_path.chmod(0o755)
        if args.open_terminal:
            subprocess.run(["open", "-a", "Terminal", str(attach_path)], check=True)

    def run(self, *command):
        return subprocess.run([*self.args, *command], capture_output=True, text=True, timeout=10)

    def capture(self):
        result = self.run("capture-pane", "-p", "-S", "-", "-t", self.target)
        if result.returncode == 0:
            (self.observations / "terminal.txt").write_text(result.stdout)
        return result.stdout

    def acknowledge_fixture_trust(self, provider, screen):
        """Only acknowledge the known, freshly generated workspace, never tool approvals."""
        if self.trust_acknowledged or str(self.workspace) not in screen:
            return
        if provider == "claude" and "Yes, I trust this folder" in screen and "No, exit" in screen:
            # Wait for the selection marker, then select and confirm on separate
            # polls. Sending both keys in one write races the TUI's state update.
            if "❯ No, exit" in screen:
                self.run("send-keys", "-t", self.target, "Down")
                return
            if "❯ Yes, I trust this folder" not in screen:
                return
            keys = ["Enter"]
        elif provider == "codex" and "Do you trust the contents of this directory?" in screen and "1. Yes, continue" in screen:
            keys = ["Enter"]
        else:
            return
        self.run("send-keys", "-t", self.target, *keys)
        self.trust_acknowledged = True
        write_json(self.observations / "startup-trust.json", {"workspace": str(self.workspace),
                   "action": "acknowledged generated fixture", "provider": provider})
        print("  Acknowledged trust for the generated fixture workspace.", flush=True)

    def close(self):
        self.capture()
        self.run("send-keys", "-t", self.target, "C-c")
        time.sleep(0.3)
        self.run("send-keys", "-t", self.target, "C-c")
        time.sleep(0.3)
        self.run("kill-server")


def run_case(args, provider, mode, case, trial, folder):
    folder.mkdir(parents=True)
    started = time.time()
    workspace = Path(tempfile.mkdtemp(prefix="ab-live-", dir="/tmp")).resolve()
    observations = workspace / ".observations"
    observations.mkdir()
    terminal = None
    process = None
    stdout = stderr = None
    trace = {"completed": False, "answer": "", "tool_calls": [], "model": None}
    error = None
    rollout = None
    env, browser_env = setup_workspace(args, provider, workspace, observations)
    with Fixture() as fixture:
        prompt = case.prompt.format(url=fixture.url)
        command = provider_command(args, provider, mode, workspace, observations, prompt, env, browser_env)
        write_json(folder / "manifest.json", {"provider": provider, "mode": mode, "case": case.id, "trial": trial,
                   "workspace": str(workspace), "argv": command, "prompt": prompt, "binary": args.binary,
                   "binary_version": args.binary_version, "provider_version": version(getattr(args, provider)),
                   "skill_sha256": hashlib.sha256(args.skill.read_bytes()).hexdigest(),
                   "claude_permission_mode": args.claude_permission_mode, "codex_auto_approve": args.codex_auto_approve,
                   "trust_workspace": args.trust_workspace})
        write_json(folder / "execution.json", {"browser_mode": args.browser_mode,
                   "sandbox_unattended": args.sandbox_unattended})
        print(f"{provider} {mode} {case.id} trial {trial}", flush=True)
        try:
            if mode == "interactive":
                terminal = Terminal(args, f"{provider}-{case.id}", workspace, observations, command, env)
            else:
                stdout = (observations / "stdout.jsonl").open("w")
                stderr = (observations / "stderr.txt").open("w")
                process = subprocess.Popen(command, cwd=workspace, env=env, stdin=subprocess.DEVNULL,
                                           stdout=stdout, stderr=stderr, start_new_session=True)
            last_progress = time.monotonic()
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline:
                if provider == "codex":
                    rollout = rollout or find_rollout(workspace, started, args.codex_root)
                trace = read_provider_trace(provider, observations, rollout)
                if trace["completed"]:
                    break
                if process is not None and process.poll() is not None:
                    # Let the provider flush its transcript after process exit.
                    trace, rollout = trace_after_flush(provider, observations, rollout,
                        lambda: find_rollout(workspace, started, args.codex_root))
                    if not trace["completed"]:
                        error = f"CLI exited with {process.returncode} before a completion event was observed"
                    break
                if (observations / "exit.json").exists():
                    # The launcher can write its exit marker between the trace
                    # poll above and this check. Give the provider time to flush
                    # its final event, then read the trace once more.
                    trace, rollout = trace_after_flush(provider, observations, rollout,
                        lambda: find_rollout(workspace, started, args.codex_root))
                    if not trace["completed"]:
                        error = "Interactive CLI exited before a completion event was observed"
                    break
                if terminal:
                    screen = terminal.capture()
                    if args.trust_workspace:
                        terminal.acknowledge_fixture_trust(provider, screen)
                if time.monotonic() - last_progress > 20:
                    hint = "inspect any pending terminal prompts" if terminal else "see observations/stdout.jsonl"
                    print(f"  Waiting for {provider} ({round(time.time() - started)}s); {hint}.", flush=True)
                    last_progress = time.monotonic()
                time.sleep(0.5)
            if not trace["completed"] and not error:
                error = f"No completion event within {args.timeout}s; inspect the terminal or provider trace"
        except KeyboardInterrupt:
            error = "Interrupted by operator"
            args.interrupted = True
        except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
            error = str(exc)
        finally:
            if terminal:
                terminal.close()
            if process is not None and process.poll() is None:
                import signal
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
            for stream in (stdout, stderr):
                if stream:
                    stream.close()
        commands = read_jsonl(observations / "commands.jsonl")
        hook_events = read_jsonl(observations / "hooks.jsonl")
        stream_events = read_jsonl(observations / "stdout.jsonl")
        if provider == "claude" and not trace["model"]:
            trace["model"] = next((row.get("model") for row in stream_events
                                   if row.get("type") == "system" and row.get("model")), args.claude_model)
        permission_modes = sorted({row["permission_mode"] for row in hook_events if row.get("permission_mode")})
        permission_denials = [row for row in stream_events if row.get("type") == "system" and row.get("subtype") == "permission_denied"]
        sandboxed = os.environ.get("AGENT_BROWSER_EVAL_SANDBOX") == "1"
        assessment = grade(case, workspace, fixture.url, fixture.heading, commands, trace["tool_calls"],
                           fixture.events, trace["answer"], trace["completed"] and not error,
                           execute_agent_code=sandboxed)
        if sandboxed and case.browser:
            launches = [p for c in commands for p in c.get("browser_processes", [])]
            assessment["checks"]["requested_browser_mode_observed"] = bool(launches) and all(
                p["headed"] == (args.browser_mode == "headed") for p in launches)
            assessment["passed"] = all(assessment["checks"].values())
        transcript = rollout if provider == "codex" else trace.get("transcript_path")
        transcript_rows = read_jsonl(transcript) if transcript else []
        observed_sources = skill_sources(transcript_rows, trace["tool_calls"])
        project_skill = workspace / (".claude" if provider == "claude" else ".agents") / "skills/agent-browser/SKILL.md"
        result = {"provider": provider, "mode": mode, "case": case.id, "trial": trial,
                  "browser": args.browser_mode,
                  "duration_seconds": round(time.time() - started, 2), "error": error,
                  "model": trace["model"], "observed_permission_modes": permission_modes,
                  "permission_denials": len(permission_denials), "observed_skill_sources": observed_sources,
                  "other_skill_source_loaded": any(source != str(project_skill) for source in observed_sources), **assessment}
        write_json(folder / "result.json", result)
        write_json(folder / "tool-calls.json", trace["tool_calls"])
        write_json(folder / "page-events.json", fixture.events)
        (folder / "answer.txt").write_text(trace["answer"])
        if transcript and Path(transcript).is_file():
            shutil.copyfile(transcript, folder / "transcript.jsonl")
        # Shutdown only sessions in this run's private socket directory.
        sessions = {"live-eval", *(c.get("session", "live-eval") for c in commands)}
        for session in sessions:
            try:
                subprocess.run([args.binary, "--session", session, "close"], env={**env, **browser_env},
                               capture_output=True, timeout=15)
            except (OSError, subprocess.TimeoutExpired):
                pass
        shutil.copytree(observations, folder / "observations")
        shutil.copytree(workspace, folder / "workspace", ignore=shutil.ignore_patterns("sockets", "tmux.sock", ".git", ".observations", "__pycache__", "node_modules", ".venv"))
    shutil.rmtree(workspace)
    print(f"  {'PASS' if result['passed'] else 'FAIL'} {result['duration_seconds']}s "
          + ", ".join(key for key, passed in result["checks"].items() if not passed), flush=True)
    return result


def comparison(results):
    pairs = []
    keys = {(r["provider"], r["case"], r["trial"]) for r in results}
    for provider, case, trial in sorted(keys):
        modes = {r["mode"]: r for r in results if (r["provider"], r["case"], r["trial"]) == (provider, case, trial)}
        if "interactive" in modes and "headless" in modes:
            pairs.append({"provider": provider, "case": case, "trial": trial,
                          "interactive_passed": modes["interactive"]["passed"], "headless_passed": modes["headless"]["passed"],
                          "different_outcome": modes["interactive"]["passed"] != modes["headless"]["passed"]})
    return pairs


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=("claude", "codex", "both"), default="both")
    parser.add_argument("--mode", choices=("interactive", "headless", "paired"), default="interactive")
    parser.add_argument("--browser-mode", choices=("headed", "headless"), default="headless")
    parser.add_argument("--sandbox-unattended", action="store_true", help="Unattended provider permissions, restricted to the disposable eval guest")
    parser.add_argument("--case", action="append", choices=[case.id for case in CASES], dest="cases")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--timeout", type=float, default=180, help="Seconds per case, including permission prompts")
    parser.add_argument("--results", type=Path, default=ROOT / "evals/results" / ("live-" + datetime.now().strftime("%Y%m%d-%H%M%S")))
    parser.add_argument("--binary", default=str(ROOT / "cli/target/debug/agent-browser") if os.access(ROOT / "cli/target/debug/agent-browser", os.X_OK) else "agent-browser")
    parser.add_argument("--chrome", help="Optional Chrome executable override")
    parser.add_argument("--skill", type=Path, default=ROOT / "skills/agent-browser/SKILL.md")
    parser.add_argument("--skills-dir", type=Path, default=ROOT / "skill-data")
    parser.add_argument("--claude", default="claude")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--claude-model", help="Omit to use the user's configured model")
    parser.add_argument("--codex-model", help="Omit to use the user's configured model")
    parser.add_argument("--claude-permission-mode", choices=("manual", "auto", "acceptEdits", "plan", "dontAsk"), help="Use the same permission mode for both transports")
    parser.add_argument("--codex-auto-approve", action="store_true", help="Use Codex automatic approval review for both transports")
    parser.add_argument("--open-terminal", action="store_true", help="Open each tmux session in macOS Terminal")
    parser.add_argument("--trust-workspace", action="store_true", help="Acknowledge startup trust only for generated fixture workspaces; never approve tool calls")
    parser.add_argument("--list", action="store_true", help="List the live cases without launching a provider")
    args = parser.parse_args(argv)
    if args.list:
        for case in CASES:
            print(f"{case.id:20} {'browser' if case.browser else 'local':8} {case.description}")
        return 0
    if args.runs < 1 or args.timeout <= 0:
        parser.error("--runs and --timeout must be positive")
    if args.sandbox_unattended and os.environ.get("AGENT_BROWSER_EVAL_SANDBOX") != "1":
        parser.error("--sandbox-unattended is only available inside the eval sandbox")
    if args.open_terminal and (sys.platform != "darwin" or args.mode == "headless"):
        parser.error("--open-terminal requires macOS and an interactive mode")
    providers = ["claude", "codex"] if args.provider == "both" else [args.provider]
    modes = ["interactive", "headless"] if args.mode == "paired" else [args.mode]
    try:
        args.binary = executable(args.binary)
        args.tmux = executable("tmux") if "interactive" in modes else None
        for provider in providers:
            setattr(args, provider, executable(getattr(args, provider)))
        if args.chrome:
            args.chrome = executable(args.chrome)
        if not args.skill.is_file() or not args.skills_dir.is_dir():
            raise ValueError("The installed skill and runtime skill directory must exist")
    except ValueError as exc:
        parser.error(str(exc))
    args.results = args.results.resolve()
    try:
        args.results.mkdir(parents=True, exist_ok=False)
    except FileExistsError:
        parser.error(f"Results directory already exists: {args.results}; choose a new directory")
    args.codex_root = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
    args.binary_version = version(args.binary)
    results = []
    args.interrupted = False
    cases = [case for case in CASES if not args.cases or case.id in args.cases]
    print(f"Results: {args.results}", flush=True)
    for trial in range(1, args.runs + 1):
        for provider in providers:
            for case in cases:
                # Alternate paired order across trials to reduce order effects.
                for mode in modes if trial % 2 else list(reversed(modes)):
                    folder = args.results / f"{provider}-{mode}-{case.id}-{trial}"
                    result = run_case(args, provider, mode, case, trial, folder)
                    results.append(result)
                    write_json(args.results / "results.json", {"results": results, "comparisons": comparison(results)})
                    if args.interrupted:
                        return 130
    passed = sum(result["passed"] for result in results)
    print(f"Passed {passed}/{len(results)}. Report: {args.results / 'results.json'}")
    return 0 if passed == len(results) else 1
