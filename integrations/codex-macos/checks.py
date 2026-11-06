#!/usr/bin/env python3
"""Offline checks using invented files; never connect to a real browser."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest import mock

sys.dont_write_bytecode = True
DIRECTORY = Path(__file__).resolve().parent
REPOSITORY = DIRECTORY.parents[1]
spec = importlib.util.spec_from_file_location('browser_setup', DIRECTORY / 'setup.py')
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class InstallerChecks(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix='codex-macos-check-')
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.root = self.base / 'runtime'
        self.codex = self.base / '.codex'
        self.codex.mkdir()
        self.chrome = self.base / 'fake-chrome'
        self.chrome.touch()

    def prepare(self, apply=False, run=None):
        def runtime(root):
            binary = root / 'bin' / 'agent-browser'
            binary.write_text('#!/bin/sh\nexit 0\n')
            return binary

        args = ['setup.py', '--root', str(self.root)]
        if apply:
            args.append('--apply')
        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.object(setup, 'CHROME', self.chrome))
            stack.enter_context(mock.patch.object(setup, 'prepare_runtime', side_effect=runtime))
            stack.enter_context(mock.patch.object(setup.shutil, 'which', return_value='/mock/codex'))
            stack.enter_context(mock.patch.object(Path, 'home', return_value=self.base))
            stack.enter_context(mock.patch.object(setup.os, 'environ', {}))
            stack.enter_context(mock.patch.object(sys, 'argv', args))
            runner = stack.enter_context(mock.patch.object(setup.subprocess, 'run', side_effect=run))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            setup.main()
        return runner

    def test_backup_preserves_bytes_hashes_and_private_permissions(self):
        originals = {'config.toml': b'model = "fixture"\n', 'AGENTS.md': b'Keep this guidance.\n'}
        for name, content in originals.items():
            (self.codex / name).write_bytes(content)
        backup = setup.backup_config(self.root, self.codex)
        manifest = json.loads((backup / 'manifest.json').read_text())
        self.assertEqual(len(manifest['files']), 2)
        self.assertEqual(stat.S_IMODE(backup.stat().st_mode), 0o700)
        for item in manifest['files']:
            target = Path(item['backup'])
            original = originals[target.name]
            self.assertEqual(target.read_bytes(), original)
            self.assertEqual((self.codex / target.name).read_bytes(), original)
            self.assertEqual(item['sha256'], hashlib.sha256(original).hexdigest())
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)

    def test_backup_handles_absent_configuration(self):
        backup = setup.backup_config(self.root, self.codex)
        self.assertEqual(json.loads((backup / 'manifest.json').read_text())['files'], [])

    def test_prepare_keeps_codex_untouched_and_selects_distinct_connections(self):
        original = b'model = "fixture"\n'
        config = self.codex / 'config.toml'
        config.write_bytes(original)
        runner = self.prepare()
        runner.assert_not_called()
        self.assertEqual(config.read_bytes(), original)
        self.assertFalse((self.codex / 'AGENTS.md').exists())
        chrome = json.loads((self.root / 'configs/chrome.json').read_text())
        isolated = json.loads((self.root / 'configs/isolated.json').read_text())
        self.assertEqual(chrome['cdp'], 'ws://127.0.0.1:9222/devtools/browser')
        self.assertTrue(chrome['pinTab'])
        self.assertNotIn('cdp', isolated)
        self.assertEqual(isolated['executablePath'], str(self.chrome))
        for item in (chrome, isolated):
            self.assertNotIn('state', item)
            self.assertNotIn('restore', item)
        subprocess.run(['bash', '-n', str(self.root / 'bin/run-mcp.sh')], check=True)

    def test_apply_backs_up_before_registration_and_preserves_other_settings(self):
        original = 'model = "fixture"\n[mcp_servers.existing]\ncommand = "existing-tool"\n'
        config = self.codex / 'config.toml'
        config.write_text(original)
        guidance = self.codex / 'AGENTS.md'
        guidance.write_text('Original guidance.\n')

        def register(command, **kwargs):
            backups = list((self.root / 'backups').glob('*/config.toml'))
            self.assertEqual(len(backups), 1)
            self.assertEqual(backups[0].read_text(), original)
            self.assertEqual(command[1:3], ['mcp', 'add'])
            name, wrapper, mode = command[3], command[5], command[6]
            with config.open('a') as output:
                output.write(f'\n[mcp_servers.{name}]\ncommand = {json.dumps(wrapper)}\nargs = [{json.dumps(mode)}]\n')

        runner = self.prepare(apply=True, run=register)
        self.assertEqual(runner.call_count, 2)
        after = tomllib.loads(config.read_text())
        self.assertEqual(after['model'], 'fixture')
        self.assertEqual(after['mcp_servers']['existing'], {'command': 'existing-tool'})
        self.assertEqual(set(after['mcp_servers']), {'existing', 'browser-isolated', 'browser-chrome'})
        self.assertTrue(guidance.read_text().startswith('Original guidance.\n'))
        self.assertEqual(guidance.read_text().count('## Codex browser connections'), 1)

    def test_apply_refuses_a_conflicting_mcp_entry(self):
        original = '[mcp_servers.browser-chrome]\ncommand = "another-tool"\n'
        config = self.codex / 'config.toml'
        config.write_text(original)
        with self.assertRaisesRegex(SystemExit, 'already has another configuration'):
            self.prepare(apply=True)
        self.assertEqual(config.read_text(), original)

    def test_checksum_failure_keeps_existing_binary(self):
        binary = self.root / 'bin' / 'agent-browser'
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b'existing fixture binary')
        with mock.patch.object(setup.sys, 'platform', 'darwin'), \
                mock.patch.object(setup.platform, 'machine', return_value='arm64'), \
                mock.patch.object(setup, 'fetch', side_effect=lambda url, target: target.write_bytes(b'invalid download')), \
                mock.patch.object(setup.subprocess, 'run') as run:
            with self.assertRaisesRegex(SystemExit, 'checksum mismatch'):
                setup.prepare_runtime(self.root)
        run.assert_not_called()
        self.assertEqual(binary.read_bytes(), b'existing fixture binary')


class PublicationChecks(unittest.TestCase):
    def test_tracked_files_and_common_sensitive_patterns(self):
        allowed = {'README.md', 'setup.py', 'checks.py'}
        names = subprocess.check_output(
            ['git', 'ls-files', '-z', '--', 'integrations/codex-macos/'],
            cwd=REPOSITORY,
        ).decode().split('\0')
        paths = [Path(name) for name in names if name]
        self.assertTrue(paths)
        self.assertEqual({p.as_posix() for p in paths}, {f'integrations/codex-macos/{name}' for name in allowed})
        patterns = {
            'personal home path': r'/' + r'Users/[^/\s]+',
            'private key': r'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----',
            'GitHub token': r'\b(?:ghp_|github_pat_)[A-Za-z0-9_]{20,}',
            'service key': r'\bsk-[A-Za-z0-9_-]{20,}',
            'email address': r'\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b',
        }
        paths.append(Path('.github/workflows/codex-macos.yml'))
        for path in paths:
            content = (REPOSITORY / path).read_text()
            self.assertLess(len(content.encode()), 100_000, f'Unexpected file size: {path}')
            for category, expression in patterns.items():
                self.assertFalse(bool(re.search(expression, content)), f'{path}: possible {category}; inspect locally')


if __name__ == '__main__':
    unittest.main(verbosity=2)
