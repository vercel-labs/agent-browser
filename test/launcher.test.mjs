import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const launcherPath = join(repositoryRoot, 'bin', 'agent-browser.js');

async function runLauncher(t, { platform, arch, binaries, args, lddOutput, muslLoader = false, postinstall = false }) {
  const packageRoot = await mkdtemp(join(tmpdir(), 'agent-browser-launcher-'));
  const binDirectory = join(packageRoot, 'bin');
  const copiedLauncherPath = join(binDirectory, 'agent-browser.js');
  const bootstrapPath = join(packageRoot, 'bootstrap.mjs');

  t.after(() => rm(packageRoot, { recursive: true, force: true }));

  await mkdir(binDirectory, { recursive: true });
  await writeFile(join(packageRoot, 'package.json'), '{"type":"module","version":"0.41.2"}\n');
  await mkdir(join(packageRoot, 'scripts'));
  await copyFile(join(repositoryRoot, 'scripts', 'postinstall.js'), join(packageRoot, 'scripts', 'postinstall.js'));
  await copyFile(launcherPath, copiedLauncherPath);
  await chmod(copiedLauncherPath, 0o755);
  await writeFile(
    bootstrapPath,
    `import { createRequire, syncBuiltinESMExports } from 'node:module';

const require = createRequire(import.meta.url);
const os = require('node:os');
const fs = require('node:fs');
const childProcess = require('node:child_process');
const originalExistsSync = fs.existsSync;
fs.existsSync = (path) => {
  if (String(path).startsWith('/lib/ld-musl-')) return ${JSON.stringify(muslLoader)};
  return originalExistsSync(path);
};
const originalExecSync = childProcess.execSync;
childProcess.execSync = (command, options) => {
  if (command === 'npm prefix -g') throw new Error('No global install in the fixture');
  if (command.startsWith('ldd ') && ${JSON.stringify(lddOutput !== undefined)}) {
    return ${JSON.stringify(lddOutput ?? '')};
  }
  return originalExecSync(command, options);
};
os.platform = () => ${JSON.stringify(platform)};
os.arch = () => ${JSON.stringify(arch)};
syncBuiltinESMExports();

await import(${JSON.stringify(postinstall ? './scripts/postinstall.js' : './bin/agent-browser.js')});
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

for (const arch of ['x64', 'arm64']) {
  test(`Linux ${arch} uses musl when ldd is unavailable`, async (t) => {
    const result = await runLauncher(t, {
      platform: 'linux',
      arch,
      lddOutput: '/bin/sh: ldd: not found\n',
      muslLoader: true,
      binaries: [
        { name: `agent-browser-linux-musl-${arch}`, marker: 'musl-binary-ran' },
        { name: `agent-browser-linux-${arch}`, marker: 'glibc-binary-ran' },
      ],
      args: ['--version'],
    });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim().split('\n')[0], 'musl-binary-ran');
  });
}

test('Linux glibc detection takes precedence over an installed musl loader', async (t) => {
  const result = await runLauncher(t, {
    platform: 'linux',
    arch: 'x64',
    lddOutput: 'ldd (GNU libc) 2.39\n',
    muslLoader: true,
    binaries: [
      { name: 'agent-browser-linux-musl-x64', marker: 'musl-binary-ran' },
      { name: 'agent-browser-linux-x64', marker: 'glibc-binary-ran' },
    ],
    args: ['--version'],
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim().split('\n')[0], 'glibc-binary-ran');
});

for (const arch of ['x64', 'arm64']) {
  test(`Linux ${arch} postinstall selects musl when ldd is unavailable`, async (t) => {
    const result = await runLauncher(t, {
      platform: 'linux',
      arch,
      lddOutput: '/bin/sh: ldd: not found\n',
      muslLoader: true,
      postinstall: true,
      binaries: [
        { name: `agent-browser-linux-musl-${arch}`, marker: 'musl-binary-ran' },
        { name: `agent-browser-linux-${arch}`, marker: 'glibc-binary-ran' },
      ],
      args: [],
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, new RegExp(`Native binary ready: agent-browser-linux-musl-${arch}`));
  });
}
