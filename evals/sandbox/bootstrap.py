"""Build a credential-free, source-specific Linux eval snapshot."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import urllib.request

ROOT = Path('/vercel/sandbox')
REPO = ROOT / 'repo'
PINS = json.loads((REPO / 'evals/sandbox/environment.json').read_text())


def run(*argv, **kwargs):
    print(' '.join(argv), flush=True)
    return subprocess.run(argv, check=True, **kwargs)


def download(url, path):
    urllib.request.urlretrieve(url, path)
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    # Same Chrome libraries as the shared Vercel helper, plus a PTY and display.
    packages = json.loads((ROOT / 'system-deps.json').read_text())
    run('sudo', 'dnf', 'install', '-y', '-q', *packages, 'tmux', 'xorg-x11-server-Xvfb',
        'xdpyinfo', 'python3.11', 'git', 'gcc', 'gcc-c++', 'make', 'unzip')
    run('sudo', 'ldconfig')
    bin_dir = ROOT / 'bin'
    bin_dir.mkdir(exist_ok=True)
    pnpm = bin_dir / 'pnpm'
    pnpm_archive = ROOT / 'pnpm.tar.gz'
    pnpm_hash = download(f"https://github.com/pnpm/pnpm/releases/download/v{PINS['pnpm']}/pnpm-linux-x64.tar.gz", pnpm_archive)
    run('tar', '-xzf', str(pnpm_archive), '-C', str(bin_dir))
    pnpm.chmod(0o755)
    run(str(pnpm), 'install', '--ignore-workspace', '--frozen-lockfile', '--ignore-scripts',
        cwd=REPO / 'evals/sandbox/tools')
    # Claude's pinned postinstall links the downloaded native optional package.
    run('node', str(REPO / 'evals/sandbox/tools/node_modules/@anthropic-ai/claude-code/install.cjs'))
    rustup = ROOT / 'rustup-init'
    download('https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init', rustup)
    rustup.chmod(0o755)
    run(str(rustup), '-y', '--no-modify-path', '--profile', 'minimal', '--default-toolchain', PINS['rust'])
    cargo = str(Path.home() / '.cargo/bin/cargo')
    # The committed Cargo.lock and uploaded source determine the binary.
    run(cargo, 'build', '--locked', '--profile', 'ci', '--manifest-path', str(REPO / 'cli/Cargo.toml'))
    run('cp', str(REPO / 'cli/target/ci/agent-browser'), str(bin_dir / 'agent-browser'))
    chrome_zip = ROOT / 'chrome.zip'
    chrome_hash = download(f"https://storage.googleapis.com/chrome-for-testing-public/{PINS['chrome']}/linux64/chrome-linux64.zip", chrome_zip)
    run('unzip', '-q', str(chrome_zip), '-d', str(ROOT))
    chrome_zip.unlink()
    tools_bin = REPO / 'evals/sandbox/tools/node_modules/.bin'
    versions = {}
    for name, argv in {
        'agent-browser': [str(bin_dir / 'agent-browser'), '--version'],
        'chrome': [str(ROOT / 'chrome-linux64/chrome'), '--version'],
        'claude': [str(tools_bin / 'claude'), '--version'],
        'codex': [str(tools_bin / 'codex'), '--version'],
        'python': ['python3.11', '--version'], 'tmux': ['tmux', '-V'],
        'node': ['node', '--version'], 'rust': [str(Path.home() / '.cargo/bin/rustc'), '--version'],
    }.items():
        versions[name] = subprocess.check_output(argv, text=True).strip()
    versions['packages'] = subprocess.check_output(['rpm', '-qa'], text=True).splitlines()
    versions['packages'].sort()
    versions['binary_sha256'] = hashlib.sha256((bin_dir / 'agent-browser').read_bytes()).hexdigest()
    versions['chrome_zip_sha256'] = chrome_hash
    versions['pnpm_sha256'] = pnpm_hash
    (ROOT / 'environment.json').write_text(json.dumps({'pins': PINS, 'versions': versions}, indent=2) + '\n')
    print(json.dumps({k: v for k, v in versions.items() if k != 'packages'}, indent=2), flush=True)


if __name__ == '__main__':
    main()
