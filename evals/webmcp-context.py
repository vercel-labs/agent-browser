#!/usr/bin/env python3
"""Run real Codex/Chrome trials with the installed skill and goal-only prompts."""
import argparse
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--chrome', required=True, type=Path)
    parser.add_argument('--codex', default='codex')
    parser.add_argument('--skills-dir', type=Path, help='Runtime skill-data directory matching the binary; defaults to this checkout.')
    parser.add_argument('--runs', type=int, default=3)
    parser.add_argument('--results', required=True, type=Path)
    parser.add_argument('--prompt', help='Optional prompt; {url} is replaced with the local shop URL.')
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    fixture = (root / 'evals/fixtures/webmcp-shop.html').read_bytes()
    args.results.mkdir(parents=True, exist_ok=True)
    results = []
    for trial in range(1, args.runs + 1):
        folder = (args.results / f'trial-{trial}').resolve()
        folder.mkdir(exist_ok=True)
        events = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *unused):
                pass

            def do_GET(self):
                self.send_response(200)
                self.send_header('Content-Type', 'text/html')
                self.send_header('Content-Length', str(len(fixture)))
                self.end_headers()
                self.wfile.write(fixture)

            def do_POST(self):
                events.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                self.send_response(204)
                self.end_headers()

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f'http://127.0.0.1:{server.server_port}'
        prompt = (args.prompt or 'Open {url}, find an in-stock blue backpack under $80, and save the cheapest match to my wishlist. Tell me the name and price. Do not purchase anything.').replace('{url}', url)
        (folder / 'prompt.txt').write_text(prompt)
        with tempfile.TemporaryDirectory(prefix='ab-eval-', dir='/tmp') as temp:
            workspace = Path(temp)
            skill = workspace / '.agents/skills/agent-browser'
            skill.mkdir(parents=True)
            shutil.copyfile(root / 'skills/agent-browser/SKILL.md', skill / 'SKILL.md')
            runtime_skills = workspace / '.agent-browser/skill-data'
            shutil.copytree(args.skills_dir or root / 'skill-data', runtime_skills)
            (workspace / 'AGENTS.md').write_text('Use the installed agent-browser skill at .agents/skills/agent-browser/SKILL.md for website interactions. The CLI is ./bin/agent-browser. Use that CLI for all website actions and observations. Do not inspect other workspaces, implementation files, or evaluation artifacts. In this host environment, run every browser command with sandbox_permissions=require_escalated so the daemon has persistent network access; the host handles approval. Skill/help commands can remain sandboxed. Complete the user task using the website.\n')
            (workspace / 'bin').mkdir()
            wrapper = workspace / 'bin/agent-browser'
            # The wrapper logs real CLI output verbatim. It sets only session
            # isolation and the Chrome executable, never WebMCP launch flags.
            wrapper.write_text('''#!/usr/bin/env python3
import json, os, subprocess, sys
env = os.environ.copy()
env['AGENT_BROWSER_SOCKET_DIR'] = %r
env['AGENT_BROWSER_EXECUTABLE_PATH'] = %r
env['AGENT_BROWSER_SESSION'] = 'webmcp-eval'
env['AGENT_BROWSER_SKILLS_DIR'] = %r
for key in ['AGENT_BROWSER_NO_WEBMCP', 'AGENT_BROWSER_CDP', 'AGENT_BROWSER_AUTO_CONNECT', 'AGENT_BROWSER_NAMESPACE', 'AGENT_BROWSER_CONFIG', 'AGENT_BROWSER_PROVIDER']:
    env.pop(key, None)
binary = %r
if 'close' in sys.argv[1:]:
    session_args = []
    if '--session' in sys.argv:
        i = sys.argv.index('--session')
        session_args = ['--session', sys.argv[i + 1]]
    probe = subprocess.run([binary, *session_args, 'get', 'text', '#wishlist', '--json'], env=env, capture_output=True, text=True, timeout=30)
    with open(%r, 'w') as verification:
        verification.write(probe.stdout)
result = subprocess.run([binary, *sys.argv[1:]], env=env, capture_output=True, text=True)
with open(%r, 'a') as log:
    log.write(json.dumps({'args': sys.argv[1:], 'stdout': result.stdout, 'stderr': result.stderr, 'exitCode': result.returncode}) + '\\n')
sys.stdout.write(result.stdout)
sys.stderr.write(result.stderr)
sys.exit(result.returncode)
''' % (str(workspace / 'sockets'), str(args.chrome.resolve()), str(runtime_skills), str(args.binary.resolve()), str(folder / 'verification.json'), str(folder / 'commands.jsonl')))
            wrapper.chmod(0o755)
            env = os.environ.copy()
            env['PATH'] = str(workspace / 'bin') + os.pathsep + env['PATH']
            env.pop('CODEX_THREAD_ID', None)
            started = time.monotonic()
            with (folder / 'agent.jsonl').open('w') as stdout, (folder / 'agent.stderr').open('w') as stderr:
                try:
                    run = subprocess.run([args.codex, 'exec', '--ephemeral', '--skip-git-repo-check', '--approve-for-me', '--json', '-C', str(workspace), '--add-dir', str(folder), '-o', str(folder / 'answer.txt'), prompt], env=env, stdout=stdout, stderr=stderr, timeout=360)
                    exit_code = run.returncode
                except subprocess.TimeoutExpired:
                    exit_code = 'timeout'
            # Independent browser observation verifies real state, not just a
            # model claim. Read after the transcript is complete and then close.
            recorded = [json.loads(line) for line in (folder / 'commands.jsonl').read_text().splitlines()] if (folder / 'commands.jsonl').exists() else []
            session = 'webmcp-eval'
            for command in recorded:
                argv = command['args']
                if command['exitCode'] == 0 and '--session' in argv:
                    session = argv[argv.index('--session') + 1]
            verification = folder / 'verification.json'
            if not verification.exists():
                check = subprocess.run([str(wrapper), '--session', session, 'get', 'text', '#wishlist', '--json'], cwd=workspace, capture_output=True, text=True, timeout=45)
                verification.write_text(check.stdout)
                subprocess.run([str(wrapper), '--session', session, 'close'], cwd=workspace, capture_output=True, timeout=45)
            verified_output = verification.read_text()
        server.shutdown()
        (folder / 'page-events.json').write_text(json.dumps(events, indent=2))
        commands = [json.loads(line) for line in (folder / 'commands.jsonl').read_text().splitlines()]
        first_open = next((c for c in commands if c['exitCode'] == 0 and ('open' in c['args'] or 'navigate' in c['args'])), {})
        invocations = [c for c in commands if 'webmcp' in c['args'] and 'invoke' in c['args']]
        list_calls = [c for c in commands if 'webmcp' in c['args'] and 'list' in c['args']]
        try:
            wishlist = json.loads(verified_output)['data']['text']
        except (ValueError, KeyError):
            wishlist = None
        result = {
            'trial': trial, 'exitCode': exit_code,
            'durationSeconds': round(time.monotonic() - started, 2),
            'skillLoaded': any(c['exitCode'] == 0 and any(c['args'][i:i + 3] == ['skills', 'get', 'core'] for i in range(len(c['args']))) for c in commands),
            'defaultNativeSupport': any(e.get('type') == 'boot' and e.get('nativeModelContext') for e in events),
            'catalogOnFirstPageLoad': 'search_products' in first_open.get('stdout', '') and 'inputSchema' in first_open.get('stdout', ''),
            'explicitDiscoveryCalls': len(list_calls),
            'webmcpInvocations': [c['args'] for c in invocations],
            'usedSearchTool': any(e.get('type') == 'search' and e.get('source') == 'webmcp' for e in events),
            'usedNewWishlistTool': any(e.get('type') == 'save' and e.get('source') == 'webmcp' and e.get('productId') == 'p1' for e in events),
            'verifiedWishlist': wishlist,
        }
        result['passed'] = all([exit_code == 0, result['skillLoaded'], result['defaultNativeSupport'], result['catalogOnFirstPageLoad'], result['explicitDiscoveryCalls'] == 0, result['usedSearchTool'], result['usedNewWishlistTool'], wishlist == 'Trail Lite Blue: $59'])
        results.append(result)
        (args.results / 'results.json').write_text(json.dumps(results, indent=2))
        print(json.dumps(result), flush=True)
    return 0 if all(r['passed'] for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
