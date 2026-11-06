import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { copyFile, mkdir, mkdtemp, readdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));

async function setup(t) {
  const tempRoot = await realpath(tmpdir());
  const packageRoot = await mkdtemp(join(tempRoot, 'agent-browser-postinstall-'));
  assert.ok(resolve(packageRoot).startsWith(resolve(tempRoot) + sep));
  t.after(() => rm(packageRoot, { recursive: true, force: true }));
  await mkdir(join(packageRoot, 'scripts'));
  await mkdir(join(packageRoot, 'bin'));
  await copyFile(join(repositoryRoot, 'scripts', 'postinstall.js'), join(packageRoot, 'scripts', 'postinstall.js'));
  await writeFile(join(packageRoot, 'package.json'), '{"type":"module","version":"0.0.0-test"}\n');
  await writeFile(join(packageRoot, 'bootstrap.mjs'), `
import { createRequire, syncBuiltinESMExports } from 'node:module';
const require = createRequire(import.meta.url);
const { EventEmitter } = require('node:events');
const { PassThrough } = require('node:stream');
const https = require('node:https');
const http = require('node:http');
const os = require('node:os');
const childProcess = require('node:child_process');
os.platform = () => process.env.MOCK_PLATFORM || 'win32';
os.arch = () => 'x64';
childProcess.execSync = () => { throw new Error('disabled in test'); };
https.get = (url, callback) => {
  if (process.env.MOCK_DOWNLOAD === 'http-fixture') {
    return http.get({ hostname: '127.0.0.1', port: Number(process.env.MOCK_PORT), path: '/asset' }, callback);
  }
  const request = new EventEmitter();
  process.nextTick(() => {
    if (process.env.MOCK_DOWNLOAD === 'network-error') {
      request.emit('error', new Error('mock request error'));
      return;
    }
    const response = new PassThrough();
    const redirect = process.env.MOCK_DOWNLOAD === 'redirect' && !url.endsWith('/asset');
    response.statusCode = redirect ? 302 : process.env.MOCK_DOWNLOAD === 'not-found' ? 404 : 200;
    response.headers = redirect ? { location: 'https://example.test/asset' } : {};
    callback(response);
    if (redirect) {
      response.end();
    } else if (process.env.MOCK_DOWNLOAD === 'held') {
      response.write('partial');
      process.stdin.once('data', () => response.end(' binary'));
      process.stdin.resume();
    } else if (process.env.MOCK_DOWNLOAD === 'interrupted' || process.env.MOCK_DOWNLOAD === 'request-interrupted') {
      response.write('partial');
      setTimeout(() => {
        if (process.env.MOCK_DOWNLOAD === 'interrupted') response.destroy(new Error('mock interrupted response'));
        else request.emit('error', new Error('mock interrupted request'));
      }, 30);
    } else {
      response.end(['success', 'redirect'].includes(process.env.MOCK_DOWNLOAD) ? 'complete binary' : '');
    }
  });
  return request;
};
syncBuiltinESMExports();
await import('./scripts/postinstall.js');
`);
  return packageRoot;
}

function runPostinstall(packageRoot, scenario, platform = 'win32') {
  return spawnSync(process.execPath, [join(packageRoot, 'bootstrap.mjs')], {
    cwd: packageRoot,
    encoding: 'utf8',
    env: { ...process.env, MOCK_DOWNLOAD: scenario, MOCK_PLATFORM: platform },
    timeout: 5000,
  });
}

function runWithFixture(packageRoot, port) {
  const child = spawn(process.execPath, [join(packageRoot, 'bootstrap.mjs')], {
    cwd: packageRoot,
    env: { ...process.env, MOCK_DOWNLOAD: 'http-fixture', MOCK_PLATFORM: 'win32', MOCK_PORT: String(port) },
    timeout: 5000,
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  return new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (status) => resolve({ status, stdout, stderr }));
  });
}

const binaryName = 'agent-browser-win32-x64.exe';

for (const [scenario, message] of [
  ['not-found', /Failed to download: HTTP 404/],
  ['network-error', /mock request error/],
  ['interrupted', /mock interrupted response/],
  ['request-interrupted', /mock interrupted request/],
  ['empty', /Downloaded native binary is empty/],
]) {
  test(`${scenario} download leaves no binary and can be retried`, async (t) => {
    const packageRoot = await setup(t);
    const binaryPath = join(packageRoot, 'bin', binaryName);
    const failed = runPostinstall(packageRoot, scenario);
    assert.equal(failed.status, 0, failed.stderr);
    assert.match(failed.stdout, message);
    assert.equal(existsSync(binaryPath), false);
    assert.equal((await readdir(join(packageRoot, 'bin'))).some((name) => name.endsWith('.tmp')), false);

    const retried = runPostinstall(packageRoot, 'success');
    assert.equal(retried.status, 0, retried.stderr);
    assert.match(retried.stdout, /Downloaded native binary/);
    assert.equal(await readFile(binaryPath, 'utf8'), 'complete binary');
  });
}

test('an existing empty binary is downloaded again', async (t) => {
  const packageRoot = await setup(t);
  const binaryPath = join(packageRoot, 'bin', binaryName);
  await writeFile(binaryPath, '');
  const result = runPostinstall(packageRoot, 'success');
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Downloaded native binary/);
  assert.equal(await readFile(binaryPath, 'utf8'), 'complete binary');
});

test('an existing nonempty binary is left in place', async (t) => {
  const packageRoot = await setup(t);
  const binaryPath = join(packageRoot, 'bin', binaryName);
  await writeFile(binaryPath, 'existing binary');
  const result = runPostinstall(packageRoot, 'network-error');
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Native binary ready/);
  assert.equal(await readFile(binaryPath, 'utf8'), 'existing binary');
});

test('a redirected download installs the response body', async (t) => {
  const packageRoot = await setup(t);
  const result = runPostinstall(packageRoot, 'redirect');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(await readFile(join(packageRoot, 'bin', binaryName), 'utf8'), 'complete binary');
});

test('Linux download installs the native binary', async (t) => {
  const packageRoot = await setup(t);
  const result = runPostinstall(packageRoot, 'success', 'linux');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(await readFile(join(packageRoot, 'bin', 'agent-browser-linux-x64'), 'utf8'), 'complete binary');
});

test('a truncated HTTP response can be retried', async (t) => {
  const packageRoot = await setup(t);
  const binaryPath = join(packageRoot, 'bin', binaryName);
  let requests = 0;
  const server = createServer((request, response) => {
    requests++;
    if (requests === 1) {
      response.writeHead(200, { 'content-length': 100 });
      response.write('partial');
      setTimeout(() => response.destroy(), 30);
    } else {
      response.end('complete binary');
    }
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const port = server.address().port;

  const failed = await runWithFixture(packageRoot, port);
  assert.equal(failed.status, 0, failed.stderr);
  assert.match(failed.stdout, /Could not download native binary/);
  assert.equal(existsSync(binaryPath), false);
  assert.equal((await readdir(join(packageRoot, 'bin'))).some((name) => name.endsWith('.tmp')), false);

  const retried = await runWithFixture(packageRoot, port);
  assert.equal(retried.status, 0, retried.stderr);
  assert.equal(await readFile(binaryPath, 'utf8'), 'complete binary');
});

test('a download is published only after its response finishes', async (t) => {
  const packageRoot = await setup(t);
  const binaryPath = join(packageRoot, 'bin', binaryName);
  const child = spawn(process.execPath, [join(packageRoot, 'bootstrap.mjs')], {
    cwd: packageRoot,
    env: { ...process.env, MOCK_DOWNLOAD: 'held', MOCK_PLATFORM: 'win32' },
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 5000,
  });
  const completion = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', resolve);
  });

  try {
    let partialExists = false;
    for (let i = 0; i < 100 && !partialExists && !existsSync(binaryPath); i++) {
      partialExists = (await readdir(join(packageRoot, 'bin'))).some((name) => name.endsWith('.tmp'));
      if (!partialExists) await new Promise((resolve) => setTimeout(resolve, 20));
    }
    assert.equal(existsSync(binaryPath), false, 'the final binary is visible before the response finishes');
    assert.equal(partialExists, true, 'the response did not start writing');
    child.stdin.end('continue');
    assert.equal(await completion, 0);
    assert.equal(await readFile(binaryPath, 'utf8'), 'partial binary');
    assert.equal((await readdir(join(packageRoot, 'bin'))).some((name) => name.endsWith('.tmp')), false);
  } finally {
    if (child.exitCode === null) {
      child.kill();
      await completion;
    }
  }
});
