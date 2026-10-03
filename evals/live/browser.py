#!/usr/bin/env python3
"""Log real CLI invocations; never fabricate a browser response for the agent."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

from capture import append_event


def session_args(args, default):
    for index, arg in enumerate(args[:-1]):
        if arg == "--session":
            return ["--session", args[index + 1]]
    return ["--session", default]


def browser_processes(binary):
    """Linux guest evidence of the actual Chrome launch mode, without child renderers."""
    if not binary or not Path('/proc').is_dir():
        return []
    rows = []
    for entry in Path('/proc').glob('[0-9]*/cmdline'):
        try:
            argv = entry.read_bytes().decode().strip('\0').split('\0')
            if argv[0] == binary and not any(arg.startswith('--type=') for arg in argv):
                rows.append({'pid': int(entry.parent.name), 'argv': argv,
                             'headed': not any(arg.startswith('--headless') for arg in argv)})
        except (OSError, UnicodeError):
            continue
    return rows


def main():
    config = json.loads((Path(__file__).parent / "browser.json").read_text())
    env = os.environ.copy()
    requested_session = env.get("AGENT_BROWSER_SESSION", config["env"]["AGENT_BROWSER_SESSION"])
    env.update(config["env"])
    env["AGENT_BROWSER_SESSION"] = requested_session
    args = sys.argv[1:]
    session = session_args(args, env.get("AGENT_BROWSER_SESSION", "live-eval"))
    invocation = str(uuid.uuid4())
    base = {"id": invocation, "args": args, "session": session[1]}
    append_event(config["log"], {**base, "event": "start", "time_ns": time.time_ns()})
    observation = None
    if "screenshot" in args:
        try:
            probe = subprocess.run([config["binary"], *session, "get", "url", "--json"],
                                   env=env, capture_output=True, text=True, timeout=30)
            observation = json.loads(probe.stdout).get("data", {}).get("url")
        except (OSError, subprocess.TimeoutExpired, ValueError, AttributeError):
            pass
    result = subprocess.run([config["binary"], *args], env=env, capture_output=True, text=True)
    append_event(config["log"], {**base, "event": "finish", "time_ns": time.time_ns(),
                 "exit_code": result.returncode, "stdout": result.stdout,
                 "stderr": result.stderr, "observed_url": observation,
                 "browser_processes": browser_processes(config["env"].get("AGENT_BROWSER_EXECUTABLE_PATH"))})
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
