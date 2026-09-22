"""Start a virtual display and configure fresh provider profiles inside the VM."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import signal

ROOT = Path('/vercel/sandbox')


def main():
    settings = json.loads((ROOT / 'trial.json').read_text())
    env = os.environ.copy()
    env['PATH'] = ':'.join([str(ROOT / 'bin'), str(ROOT / 'repo/evals/sandbox/tools/node_modules/.bin'), env['PATH']])
    env['TERM'] = 'xterm-256color'
    env['DISPLAY'] = ':99'
    env['AGENT_BROWSER_EVAL_SANDBOX'] = '1'
    # Make the pinned browser discoverable by doctor and normal cache lookup too.
    pins = json.loads((ROOT / 'environment.json').read_text())['pins']
    browser_cache = Path.home() / '.agent-browser/browsers'
    browser_cache.mkdir(parents=True, exist_ok=True)
    installed = browser_cache / ('chrome-' + pins['chrome'])
    if not installed.exists():
        installed.symlink_to(ROOT / 'chrome-linux64', target_is_directory=True)
    codex = Path.home() / '.codex'
    codex.mkdir(exist_ok=True)
    (codex / 'config.toml').write_text('''model_provider = "gateway"
check_for_update_on_startup = false
[model_providers.gateway]
name = "Vercel AI Gateway"
base_url = "https://ai-gateway.vercel.sh/v1"
env_key = "AI_GATEWAY_API_KEY"
wire_api = "responses"
''')
    # Skip first-install cosmetics and the API-key selection screen, not tool prompts.
    claude = {'hasCompletedOnboarding': True, 'theme': 'dark',
              'bypassPermissionsModeAccepted': settings['permissions'] == 'unattended'}
    if env.get('ANTHROPIC_API_KEY'):
        claude['customApiKeyResponses'] = {'approved': [env['ANTHROPIC_API_KEY'][-20:]], 'rejected': []}
    (Path.home() / '.claude.json').write_text(json.dumps(claude))
    xvfb_log = (ROOT / 'xvfb.log').open('w')
    display = subprocess.Popen(['Xvfb', ':99', '-screen', '0', '1440x900x24', '-nolisten', 'tcp', '-ac'], stdout=xvfb_log, stderr=xvfb_log)
    child = None
    def interrupt(signum, frame):
        if child is not None and child.poll() is None:
            child.send_signal(signal.SIGINT)
    signal.signal(signal.SIGTERM, interrupt)
    signal.signal(signal.SIGINT, interrupt)
    try:
        for _ in range(100):
            if subprocess.run(['xdpyinfo'], env=env, capture_output=True).returncode == 0:
                break
            if display.poll() is not None:
                raise RuntimeError('Xvfb exited; inspect xvfb.log')
            time.sleep(0.1)
        else:
            raise RuntimeError('Xvfb did not become ready')
        argv = [sys.executable, str(ROOT / 'repo/evals/live-evals.py'),
                '--provider', settings['provider'], '--mode', settings['mode'], '--case', settings['case'],
                '--browser-mode', settings['browser'], '--timeout', str(settings['timeout']),
                '--results', str(ROOT / 'results'), '--binary', str(ROOT / 'bin/agent-browser'),
                '--chrome', str(ROOT / 'chrome-linux64/chrome'), '--trust-workspace',
                '--claude-model', settings['claudeModel'], '--codex-model', settings['codexModel']]
        if settings['permissions'] == 'unattended':
            argv.append('--sandbox-unattended')
        child = subprocess.Popen(argv, env=env)
        return child.wait()
    finally:
        display.terminate()
        display.wait(timeout=5)
        xvfb_log.close()


if __name__ == '__main__':
    raise SystemExit(main())
