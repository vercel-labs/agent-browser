import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const launcherPath = join(repositoryRoot, 'bin', 'agent-browser.js');

async function runLauncher(t, { platform, arch, binaries, args }) {
  const packageRoot = await mkdtemp(join(tmpdir(), 'agent-browser-launcher-'));
  const binDirectory = join(packageRoot, 'bin');
  const copiedLauncherPath = join(binDirectory, 'agent-browser.js');
  const bootstrapPath = join(packageRoot, 'bootstrap.mjs');

  t.after(() => rm(packageRoot, { recursive: true, force: true }));

  await mkdir(binDirectory, { recursive: true });
  await writeFile(join(packageRoot, 'package.json'), '{"type":"module"}\n');
  await copyFile(launcherPath, copiedLauncherPath);
  await chmod(copiedLauncherPath, 0o755);
  await writeFile(
    bootstrapPath,
    `import { createRequire, syncBuiltinESMExports } from 'node:module';

const require = createRequire(import.meta.url);
const os = require('node:os');
os.platform = () => ${JSON.stringify(platform)};
os.arch = () => ${JSON.stringify(arch)};
syncBuiltinESMExports();

await import('./bin/agent-browser.js');
`,
  );
  for (const { name, marker } of binaries) {
    const fakeBinaryPath = join(binDirectory, name);
    await writeFile(
      fakeBinaryPath,
      `#!/bin/sh
printf '%s\\n' ${JSON.stringify(marker)} "$@"
`,
    );
    await chmod(fakeBinaryPath, 0o755);
  }

  return spawnSync(process.execPath, [bootstrapPath, ...args], {
    cwd: packageRoot,
    encoding: 'utf8',
  });
}

test('Windows ARM64 launcher uses the published x64 executable', async (t) => {
  const args = ['open', 'https://example.com'];
  const result = await runLauncher(t, {
    platform: 'win32',
    arch: 'arm64',
    binaries: [
      { name: 'agent-browser-win32-x64.exe', marker: 'x64-binary-ran' },
    ],
    args,
  });

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split('\n'), ['x64-binary-ran', ...args]);
  assert.doesNotMatch(result.stderr, /No binary found/);
});

test('Windows ARM64 launcher prefers an existing native executable', async (t) => {
  const args = ['--version'];
  const result = await runLauncher(t, {
    platform: 'win32',
    arch: 'arm64',
    binaries: [
      { name: 'agent-browser-win32-arm64.exe', marker: 'arm64-binary-ran' },
      { name: 'agent-browser-win32-x64.exe', marker: 'x64-binary-ran' },
    ],
    args,
  });

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split('\n'), ['arm64-binary-ran', ...args]);
});

test('Windows ARM64 launcher runs when only a source-built native executable exists', async (t) => {
  const args = ['snapshot'];
  const result = await runLauncher(t, {
    platform: 'win32',
    arch: 'arm64',
    binaries: [
      { name: 'agent-browser-win32-arm64.exe', marker: 'arm64-binary-ran' },
    ],
    args,
  });

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split('\n'), ['arm64-binary-ran', ...args]);
  assert.doesNotMatch(result.stderr, /No binary found/);
});

test('macOS ARM64 launcher keeps selecting the ARM64 executable', async (t) => {
  const args = ['--version'];
  const result = await runLauncher(t, {
    platform: 'darwin',
    arch: 'arm64',
    binaries: [
      { name: 'agent-browser-darwin-arm64', marker: 'arm64-binary-ran' },
    ],
    args,
  });

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split('\n'), ['arm64-binary-ran', ...args]);
});
