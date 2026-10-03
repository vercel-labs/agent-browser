import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const payload = Buffer.from('downloaded native binary fixture\n');

async function runPostinstall(t, route, respond) {
  const packageRoot = await mkdtemp(join(tmpdir(), 'agent-browser-postinstall-'));
  const requests = [];
  const server = createServer((req, res) => {
    requests.push(req.url);
    respond(req, res, origin);
  });
  // An unread redirect must not keep the installer alive until this expires.
  server.keepAliveTimeout = 60_000;
  let origin;
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
  t.after(async () => {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
    await rm(packageRoot, { recursive: true, force: true });
  });

  await mkdir(join(packageRoot, 'scripts'));
  await writeFile(join(packageRoot, 'package.json'), '{"type":"module","version":"0.0.0"}\n');
  await copyFile(
    join(repositoryRoot, 'scripts', 'postinstall.js'),
    join(packageRoot, 'scripts', 'postinstall.js')
  );
  await writeFile(
    join(packageRoot, 'bootstrap.mjs'),
    `import https from 'node:https';
import http from 'node:http';
import os from 'node:os';
import childProcess from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';

// Use real HTTP responses and sockets locally, without a GitHub dependency.
https.get = (url, callback) => http.get(
  url.startsWith('https://github.com/') ? ${JSON.stringify(origin + route)} : url,
  callback
);
os.platform = () => 'linux';
os.arch = () => 'x64';
// Keep global-install probing inside this test's temporary directory.
childProcess.execSync = (command) => {
  if (command === 'npm prefix -g') return ${JSON.stringify(packageRoot)};
  if (command === 'ldd --version 2>&1 || true') return 'glibc';
  throw new Error('Unexpected command: ' + command);
};
syncBuiltinESMExports();
await import('./scripts/postinstall.js');
`
  );

  const child = spawn(process.execPath, [join(packageRoot, 'bootstrap.mjs')], {
    cwd: packageRoot,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  let timedOut = false;
  child.stdout.on('data', (data) => (stdout += data));
  child.stderr.on('data', (data) => (stderr += data));
  const timer = setTimeout(() => {
    timedOut = true;
    child.kill();
  }, 5_000);
  const [code, signal] = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (...result) => resolve(result));
  }).finally(() => clearTimeout(timer));
  assert.equal(timedOut, false, `Installer retained a socket after download:\n${stdout}\n${stderr}`);
  assert.equal(signal, null, stderr);
  assert.equal(code, 0, stderr);
  return { stdout, requests, binaryPath: join(packageRoot, 'bin', 'agent-browser-linux-x64') };
}

function sendBinary(res) {
  res.writeHead(200, { 'Content-Length': payload.length });
  res.end(payload);
}

async function assertDownloaded(result) {
  assert.match(result.stdout, /Downloaded native binary/);
  assert.deepEqual(await readFile(result.binaryPath), payload);
  if (process.platform !== 'win32') {
    assert.equal((await stat(result.binaryPath)).mode & 0o777, 0o755);
  }
}

test('postinstall downloads a direct response and exits', async (t) => {
  const result = await runPostinstall(t, '/binary', (_req, res) => sendBinary(res));
  await assertDownloaded(result);
  assert.deepEqual(result.requests, ['/binary']);
});

for (const [status, body] of [
  [301, 'redirect body'],
  [302, ''],
]) {
  test(`postinstall drains a ${status} redirect and exits before keepalive expires`, async (t) => {
    const result = await runPostinstall(t, '/redirect', (req, res, origin) => {
      if (req.url === '/redirect') {
        res.writeHead(status, {
          Location: `${origin}/binary`,
          'Content-Length': Buffer.byteLength(body),
        });
        res.end(body);
      } else {
        sendBinary(res);
      }
    });
    await assertDownloaded(result);
    assert.deepEqual(result.requests, ['/redirect', '/binary']);
  });
}

test('postinstall drains every response in a redirect chain', async (t) => {
  const result = await runPostinstall(t, '/first', (req, res, origin) => {
    if (req.url === '/binary') return sendBinary(res);
    const first = req.url === '/first';
    res.writeHead(first ? 301 : 302, { Location: `${origin}/${first ? 'second' : 'binary'}` });
    res.end('unused redirect body');
  });
  await assertDownloaded(result);
  assert.deepEqual(result.requests, ['/first', '/second', '/binary']);
});

test('postinstall preserves HTTP failure reporting after a redirect', async (t) => {
  const result = await runPostinstall(t, '/redirect', (req, res, origin) => {
    if (req.url === '/redirect') {
      res.writeHead(302, { Location: `${origin}/missing` });
      res.end('unused redirect body');
    } else {
      // Closing the error response isolates the redirect resource regression.
      res.writeHead(404, { Connection: 'close' });
      res.end('not found');
    }
  });
  assert.match(result.stdout, /Could not download native binary: Failed to download: HTTP 404/);
  assert.doesNotMatch(result.stdout, /Downloaded native binary/);
  assert.deepEqual(result.requests, ['/redirect', '/missing']);
});

test('postinstall preserves connection failure reporting', async (t) => {
  const result = await runPostinstall(t, '/reset', (req) => req.socket.destroy());
  assert.match(result.stdout, /Could not download native binary: socket hang up/);
  assert.doesNotMatch(result.stdout, /Downloaded native binary/);
  await assert.rejects(stat(result.binaryPath), { code: 'ENOENT' });
});
