import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const postinstallPath = join(repositoryRoot, 'scripts', 'postinstall.js');
const isMusl = existsSync('/lib/ld-musl-x86_64.so.1') || existsSync('/lib/ld-musl-aarch64.so.1');
const skip = process.platform === 'win32' || isMusl;

// A package with its native binary in place, so postinstall downloads nothing.
async function makePackage(root) {
  const bin = join(root, 'bin');
  await mkdir(join(root, 'scripts'), { recursive: true });
  await mkdir(bin, { recursive: true });
  await writeFile(join(root, 'package.json'), '{"type":"module","version":"0.0.0"}\n');
  await copyFile(postinstallPath, join(root, 'scripts', 'postinstall.js'));
  await writeFile(join(bin, 'agent-browser.js'), '');
  const binary = join(bin, `agent-browser-${process.platform}-${process.arch}`);
  await writeFile(binary, '');
  return { wrapper: join(bin, 'agent-browser.js'), binary };
}

async function setup(t) {
  const root = await mkdtemp(join(tmpdir(), 'agent-browser-postinstall-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const prefix = join(root, 'global');
  await mkdir(join(prefix, 'bin'), { recursive: true });
  return { root, prefix, link: join(prefix, 'bin', 'agent-browser') };
}

function runPostinstall(packageRoot, prefix) {
  const result = spawnSync(process.execPath, [join(packageRoot, 'scripts', 'postinstall.js')], {
    cwd: packageRoot,
    env: { ...process.env, npm_config_prefix: prefix },
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stderr);
}

test('a global install points the global command at its native binary', { skip }, async (t) => {
  const { root, prefix, link } = await setup(t);
  const globalPackage = join(prefix, 'lib', 'node_modules', 'agent-browser');
  const { wrapper, binary } = await makePackage(globalPackage);
  await symlink(wrapper, link);

  runPostinstall(globalPackage, prefix);

  assert.equal(await realpath(link), await realpath(binary));
});

test('a local install leaves the global command alone', { skip }, async (t) => {
  const { root, prefix, link } = await setup(t);
  const globalPackage = join(prefix, 'lib', 'node_modules', 'agent-browser');
  const { binary: globalBinary } = await makePackage(globalPackage);
  await symlink(globalBinary, link);
  const localPackage = join(root, 'project', 'node_modules', 'agent-browser');
  await makePackage(localPackage);

  runPostinstall(localPackage, prefix);

  assert.equal(await realpath(link), await realpath(globalBinary));
});
