import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readdir, readFile, readlink, realpath, rename, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const scriptsDir = join(repositoryRoot, 'scripts');
const payload = Buffer.from('#!/bin/sh\necho downloaded\n');

async function tempDir(t, prefix) {
  const directory = await mkdtemp(join(tmpdir(), prefix));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

test('bin entry link survives the install directory being renamed', { skip: process.platform === 'win32' }, async (t) => {
  const { linkNative } = await import(pathToFileURL(join(scriptsDir, 'link-native.js')).href);
  const root = await realpath(await tempDir(t, 'agent-browser-link-'));
  const staging = join(root, 'tmp', '.tmpABC123');
  const packageRoot = join(staging, 'lib', 'node_modules', 'agent-browser');
  const binary = join(packageRoot, 'bin', 'agent-browser-darwin-arm64');
  const entry = join(staging, 'bin', 'agent-browser');
  await mkdir(join(packageRoot, 'bin'), { recursive: true });
  await mkdir(dirname(entry), { recursive: true });
  await writeFile(join(packageRoot, 'bin', 'agent-browser.js'), '');
  await writeFile(binary, payload, { mode: 0o755 });
  await symlink('../lib/node_modules/agent-browser/bin/agent-browser.js', entry);

  const changed = linkNative({ packageRoot, binary, target: { binaryName: 'agent-browser-darwin-arm64' }, entries: [entry] });
  assert.equal(changed, true);
  assert.equal(isAbsolute(await readlink(entry)), false);

  const final = join(root, 'tools');
  await rename(staging, final);
  const resolved = await realpath(join(final, 'bin', 'agent-browser'));
  assert.equal(resolved, join(final, 'lib', 'node_modules', 'agent-browser', 'bin', 'agent-browser-darwin-arm64'));
});

test('an empty platform package binary is skipped and named by the launcher', { skip: process.platform === 'win32' }, async (t) => {
  const packageRoot = await tempDir(t, 'agent-browser-empty-pkg-');
  const { nativeTarget } = await import(pathToFileURL(join(scriptsDir, 'native.js')).href);
  const target = nativeTarget();
  await mkdir(join(packageRoot, 'bin'));
  await copyFile(join(repositoryRoot, 'bin', 'agent-browser.js'), join(packageRoot, 'bin', 'agent-browser.js'));
  await mkdir(join(packageRoot, 'scripts'));
  for (const name of ['native.js', 'link-native.js']) {
    await copyFile(join(scriptsDir, name), join(packageRoot, 'scripts', name));
  }
  await writeFile(join(packageRoot, 'package.json'), '{"type":"module"}\n');
  const platformDir = join(packageRoot, 'node_modules', ...target.packageName.split('/'));
  await mkdir(join(platformDir, 'bin'), { recursive: true });
  await writeFile(join(platformDir, 'package.json'), JSON.stringify({ name: target.packageName }));
  const emptyBinary = join(platformDir, 'bin', target.file);
  await writeFile(emptyBinary, '');

  const { findNativeBinary, emptyNativeBinaries } = await import(pathToFileURL(join(packageRoot, 'scripts', 'native.js')).href);
  assert.equal(findNativeBinary(packageRoot, target), null);
  assert.deepEqual(emptyNativeBinaries(packageRoot, target), [await realpath(emptyBinary)]);

  const result = spawnSync(process.execPath, [join(packageRoot, 'bin', 'agent-browser.js')], {
    encoding: 'utf8',
    env: { ...process.env },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /0 bytes/);
  assert.match(result.stderr, new RegExp(target.file.replace('.', '\\.')));
});

test('findNativeBinary skips empty candidates', async (t) => {
  const { findNativeBinary } = await import(pathToFileURL(join(scriptsDir, 'native.js')).href);
  const packageRoot = await tempDir(t, 'agent-browser-native-');
  await mkdir(join(packageRoot, 'bin'));
  const target = { packageName: '@agent-browser/none', candidates: ['empty', 'valid'] };

  await writeFile(join(packageRoot, 'bin', 'empty'), '');
  assert.equal(findNativeBinary(packageRoot, target), null);

  await writeFile(join(packageRoot, 'bin', 'valid'), payload);
  assert.equal(findNativeBinary(packageRoot, target), join(packageRoot, 'bin', 'valid'));
});

// A regression must fail red, not hang CI.
const POSTINSTALL_TIMEOUT_MS = 30_000;
test.postinstall = (name, fn) => test(name, { timeout: POSTINSTALL_TIMEOUT_MS }, fn);

// Runs postinstall in a temp package with the GitHub download pointed at a
// local server. `respond` handles every request the installer makes.
async function runPostinstall(t, respond) {
  const packageRoot = await tempDir(t, 'agent-browser-postinstall-');
  let origin;
  const server = createServer((req, res) => respond(req, res));
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
  t.after(async () => {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  });

  await mkdir(join(packageRoot, 'scripts'));
  await mkdir(join(packageRoot, 'bin'));
  await writeFile(join(packageRoot, 'package.json'), '{"type":"module","version":"0.0.0"}\n');
  for (const name of ['postinstall.js', 'native.js', 'link-native.js']) {
    await copyFile(join(scriptsDir, name), join(packageRoot, 'scripts', name));
  }
  await writeFile(
    join(packageRoot, 'bootstrap.mjs'),
    `import https from 'node:https';
import http from 'node:http';
import os from 'node:os';
import childProcess from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';

const realGet = https.get;
// Only the github.com origin is served locally; the path is kept.
https.get = (url, callback) =>
  url.startsWith('https://github.com/')
    ? http.get(url.replace('https://github.com', ${JSON.stringify(origin)}), callback)
    : realGet(url, callback);
os.platform = () => 'darwin';
os.arch = () => 'arm64';
childProcess.execSync = () => {
  throw new Error('no subprocesses in tests');
};
syncBuiltinESMExports();
await import('./scripts/postinstall.js');
`,
  );

  const child = spawn(process.execPath, [join(packageRoot, 'bootstrap.mjs')], {
    cwd: packageRoot,
    env: { PATH: process.env.PATH },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  child.stdout.on('data', (data) => (stdout += data));
  child.stderr.on('data', (data) => (stdout += data));
  t.after(() => child.kill());
  const status = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', resolve);
  });
  assert.equal(status, 0, stdout);
  const binDir = join(packageRoot, 'bin');
  return { stdout, binDir, files: await readdir(binDir), binary: join(binDir, 'agent-browser-darwin-arm64') };
}

test.postinstall('a 404 download leaves no file in bin/', async (t) => {
  const result = await runPostinstall(t, (_req, res) => res.writeHead(404).end('missing'));
  assert.match(result.stdout, /Could not download native binary/);
  assert.deepEqual(result.files, []);
});

test.postinstall('an empty 200 download leaves no file in bin/', async (t) => {
  const result = await runPostinstall(t, (_req, res) => res.writeHead(200).end());
  assert.deepEqual(result.files, []);
});

test.postinstall('a truncated download leaves no file in bin/', async (t) => {
  const result = await runPostinstall(t, (_req, res) => {
    res.writeHead(200, { 'Content-Length': 1000 });
    res.write('partial');
    setImmediate(() => res.destroy());
  });
  assert.deepEqual(result.files, []);
});

const serveFinal = (redirect) => (req, res) => {
  if (req.url === '/final') {
    res.writeHead(200).end(payload);
  } else {
    redirect(req, res);
  }
};

async function assertInstalled(result) {
  assert.deepEqual(result.files.filter((name) => !name.startsWith('.')), ['agent-browser-darwin-arm64']);
  assert.deepEqual(await readFile(result.binary), payload);
  assert.equal((await stat(result.binary)).mode & 0o111, 0o111);
}

for (const status of [301, 302, 307, 308]) {
  for (const location of ['/final', 'https://github.com/final']) {
    test.postinstall(`download follows a ${status} redirect to ${location}`, async (t) => {
      const result = await runPostinstall(t, serveFinal((_req, res) => res.writeHead(status, { Location: location }).end()));
      await assertInstalled(result);
    });
  }
}

test.postinstall('a redirect to a non-https URL fails cleanly and leaves no file', async (t) => {
  const result = await runPostinstall(t, (_req, res) => res.writeHead(302, { Location: 'http://127.0.0.1:1/final' }).end());
  assert.match(result.stdout, /Could not download native binary/);
  assert.deepEqual(result.files, []);
});

test.postinstall('an invalid redirect Location fails cleanly and leaves no file', async (t) => {
  const result = await runPostinstall(t, (_req, res) => res.writeHead(302, { Location: 'http://' }).end());
  assert.match(result.stdout, /Could not download native binary/);
  assert.deepEqual(result.files, []);
});

test.postinstall('20 redirects are followed and a 21st fails', async (t) => {
  let hops = 0;
  const ok = await runPostinstall(t, (req, res) => {
    if (req.url === '/final') return res.writeHead(200).end(payload);
    if (++hops > 20) return res.writeHead(500).end();
    res.writeHead(302, { Location: hops === 20 ? '/final' : '/hop' }).end();
  });
  await assertInstalled(ok);

  const loop = await runPostinstall(t, (_req, res) => res.writeHead(302, { Location: '/loop' }).end());
  assert.match(loop.stdout, /more than 20 redirects/);
  assert.deepEqual(loop.files, []);
});
