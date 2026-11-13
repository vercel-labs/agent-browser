import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmod, copyFile, mkdir, mkdtemp, readFile, readlink, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const launcherPath = join(repositoryRoot, 'bin', 'agent-browser.js');
const sharedModules = ['native.js', 'link-native.js'];

// Creates a package directory holding the launcher and the modules it imports.
async function createPackage(t, packageRoot) {
  if (!packageRoot) {
    packageRoot = await mkdtemp(join(tmpdir(), 'agent-browser-launcher-'));
    t.after(() => rm(packageRoot, { recursive: true, force: true }));
  }
  await mkdir(join(packageRoot, 'bin'), { recursive: true });
  await mkdir(join(packageRoot, 'scripts'), { recursive: true });
  await writeFile(join(packageRoot, 'package.json'), '{"type":"module"}\n');
  await copyFile(launcherPath, join(packageRoot, 'bin', 'agent-browser.js'));
  await chmod(join(packageRoot, 'bin', 'agent-browser.js'), 0o755);
  for (const name of sharedModules) {
    await copyFile(join(repositoryRoot, 'scripts', name), join(packageRoot, 'scripts', name));
  }
  return packageRoot;
}

async function writeFakeBinary(path, marker) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(
    path,
    `#!/bin/sh
printf '%s\\n' ${JSON.stringify(marker)} "$@"
`,
  );
  await chmod(path, 0o755);
}

// Installs a fake platform package, as npm does for optionalDependencies.
async function writePlatformPackage(packageRoot, packageName, file, marker) {
  const directory = join(packageRoot, 'node_modules', ...packageName.split('/'));
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, 'package.json'), JSON.stringify({ name: packageName }));
  await writeFakeBinary(join(directory, 'bin', file), marker);
  return join(directory, 'bin', file);
}

async function runLauncher(t, { platform, arch, binaries = [], platformPackage, args }) {
  const packageRoot = await createPackage(t);
  const bootstrapPath = join(packageRoot, 'bootstrap.mjs');

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
    await writeFakeBinary(join(packageRoot, 'bin', name), marker);
  }
  if (platformPackage) {
    await writePlatformPackage(packageRoot, platformPackage.name, platformPackage.file, platformPackage.marker);
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

test('Linux launcher runs the binary from the platform package', async (t) => {
  const args = ['--version'];
  const result = await runLauncher(t, {
    platform: 'linux',
    arch: 'x64',
    platformPackage: { name: '@agent-browser/cli-linux-x64-gnu', file: 'agent-browser', marker: 'platform-package-ran' },
    args,
  });

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split('\n'), ['platform-package-ran', ...args]);
});

test('Windows launcher runs the binary from the platform package', async (t) => {
  const args = ['open', 'https://example.com'];
  const result = await runLauncher(t, {
    platform: 'win32',
    arch: 'arm64',
    platformPackage: { name: '@agent-browser/cli-win32-x64-msvc', file: 'agent-browser.exe', marker: 'win32-package-ran' },
    args,
  });

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split('\n'), ['win32-package-ran', ...args]);
});

test('launcher reports a missing platform package', async (t) => {
  const result = await runLauncher(t, { platform: 'darwin', arch: 'arm64', args: ['--version'] });

  assert.equal(result.status, 1);
  assert.match(result.stderr, /@agent-browser\/cli-darwin-arm64/);
});

// The remaining tests run the launcher on the real host, the way a package
// manager's bin entry does, so they only apply where bin links are symlinks.
const posix = process.platform !== 'win32';

async function hostTarget() {
  const { nativeTarget } = await import('../scripts/native.js');
  return nativeTarget();
}

test('first run through a bin symlink relinks it to the native binary', { skip: !posix }, async (t) => {
  const target = await hostTarget();
  const prefix = await mkdtemp(join(tmpdir(), 'agent-browser-global-'));
  t.after(() => rm(prefix, { recursive: true, force: true }));
  const packageRoot = await createPackage(t, join(prefix, 'lib', 'node_modules', 'agent-browser'));
  await writePlatformPackage(packageRoot, target.packageName, target.file, 'native-ran');
  const entry = join(prefix, 'bin', 'agent-browser');
  await mkdir(dirname(entry), { recursive: true });
  await symlink('../lib/node_modules/agent-browser/bin/agent-browser.js', entry);

  const first = spawnSync(entry, ['--version'], { encoding: 'utf8' });
  assert.equal(first.status, 0, first.stderr);
  assert.deepEqual(first.stdout.trim().split('\n'), ['native-ran', '--version']);

  // The link now targets a copy inside this package's own bin directory, so
  // package managers still recognize and remove it on uninstall.
  const linkedBinary = join(await realpath(packageRoot), 'bin', target.binaryName);
  assert.equal(await readlink(entry), linkedBinary);

  const second = spawnSync(entry, ['snapshot'], { encoding: 'utf8' });
  assert.equal(second.status, 0, second.stderr);
  assert.deepEqual(second.stdout.trim().split('\n'), ['native-ran', 'snapshot']);
});

test('linkNative leaves bin entries that do not target the launcher', { skip: !posix }, async (t) => {
  const target = await hostTarget();
  const { linkNative } = await import('../scripts/link-native.js');
  const packageRoot = await realpath(await createPackage(t));
  const binary = await writePlatformPackage(packageRoot, target.packageName, target.file, 'native-ran');

  const otherPackage = await createPackage(t);
  const foreignLink = join(packageRoot, 'foreign-link');
  await symlink(join(otherPackage, 'bin', 'agent-browser.js'), foreignLink);
  const regularFile = join(packageRoot, 'regular-file');
  await copyFile(launcherPath, regularFile);

  const changed = linkNative({ packageRoot, binary, target, entries: [foreignLink, regularFile] });

  assert.equal(changed, false);
  assert.equal(await readlink(foreignLink), join(otherPackage, 'bin', 'agent-browser.js'));
  assert.equal(await readFile(regularFile, 'utf8'), await readFile(launcherPath, 'utf8'));
});

test('linkNative rewrites the pnpm shim that runs this launcher', { skip: !posix }, async (t) => {
  const target = await hostTarget();
  const { linkNative } = await import('../scripts/link-native.js');
  const pnpmHome = await mkdtemp(join(tmpdir(), 'agent-browser-pnpm-'));
  t.after(() => rm(pnpmHome, { recursive: true, force: true }));
  const packageRoot = await createPackage(t, join(pnpmHome, 'global', 'v11', 'abc123', 'node_modules', 'agent-browser'));
  const binary = await writePlatformPackage(packageRoot, target.packageName, target.file, 'native-ran');
  const launcher = join(packageRoot, 'bin', 'agent-browser.js');

  const shim = join(pnpmHome, 'bin', 'agent-browser');
  await mkdir(dirname(shim), { recursive: true });
  await writeFile(shim, `#!/bin/sh\nexec node "${launcher}" "$@"\n# cmd-shim-target=${launcher}\n`, { mode: 0o755 });

  assert.equal(linkNative({ packageRoot, binary, target, entries: [launcher] }), true);
  const content = await readFile(shim, 'utf8');
  assert.match(content, new RegExp(`^# cmd-shim-target=${launcher.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`, 'm'));
  assert.doesNotMatch(content, /exec node/);

  const result = spawnSync(shim, ['--version'], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split('\n'), ['native-ran', '--version']);
});

test('linkNative leaves a pnpm shim for another target untouched', { skip: !posix }, async (t) => {
  const target = await hostTarget();
  const { linkNative } = await import('../scripts/link-native.js');
  const pnpmHome = await mkdtemp(join(tmpdir(), 'agent-browser-pnpm-'));
  t.after(() => rm(pnpmHome, { recursive: true, force: true }));
  const packageRoot = await createPackage(t, join(pnpmHome, 'global', 'v11', 'abc123', 'node_modules', 'agent-browser'));
  const binary = await writePlatformPackage(packageRoot, target.packageName, target.file, 'native-ran');

  const shim = join(pnpmHome, 'bin', 'agent-browser');
  const original = '#!/bin/sh\nexec node "/elsewhere/agent-browser.js" "$@"\n# cmd-shim-target=/elsewhere/agent-browser.js\n';
  await mkdir(dirname(shim), { recursive: true });
  await writeFile(shim, original, { mode: 0o755 });

  linkNative({ packageRoot, binary, target, entries: [join(packageRoot, 'bin', 'agent-browser.js')] });
  assert.equal(await readFile(shim, 'utf8'), original);
});
