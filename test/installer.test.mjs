import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { downloadFile } from '../scripts/download.js';

const root = dirname(dirname(fileURLToPath(import.meta.url)));

async function directory(t) {
  const path = await mkdtemp(join(tmpdir(), 'agent-browser-download-'));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

async function server(t, handler) {
  const http = createServer(handler);
  await new Promise((resolve) => http.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => {
    http.closeAllConnections();
    http.close(resolve);
  }));
  return `http://127.0.0.1:${http.address().port}`;
}

test('HTTP errors do not publish files or poison the next download', async (t) => {
  const dir = await directory(t);
  const destination = join(dir, 'binary');
  let status = 404;
  const url = await server(t, (_request, response) => {
    response.writeHead(status);
    response.end(status === 200 ? 'complete binary' : 'unavailable');
  });
  await assert.rejects(downloadFile(url, destination), /HTTP 404/);
  assert.deepEqual(await readdir(dir), []);
  status = 200;
  await downloadFile(url, destination);
  assert.equal(await readFile(destination, 'utf8'), 'complete binary');
  assert.deepEqual(await readdir(dir), ['binary']);
});

test('relative redirects are followed and redirect loops are bounded', async (t) => {
  const dir = await directory(t);
  const destination = join(dir, 'binary');
  const url = await server(t, (request, response) => {
    if (request.url === '/binary') {
      response.end('complete binary');
    } else {
      response.writeHead(302, { Location: request.url === '/loop' ? '/loop' : '/binary' });
      response.end();
    }
  });
  await assert.rejects(downloadFile(`${url}/loop`, destination, { maxRedirects: 2 }), /redirects/);
  assert.deepEqual(await readdir(dir), []);
  await downloadFile(`${url}/start`, destination);
  assert.equal(await readFile(destination, 'utf8'), 'complete binary');
});

for (const mode of ['empty', 'truncated', 'disconnected', 'timeout']) {
  test(`${mode} downloads preserve the existing file and clean staging files`, async (t) => {
    const dir = await directory(t);
    const destination = join(dir, 'binary');
    await writeFile(destination, 'usable previous binary');
    const url = await server(t, (_request, response) => {
      if (mode === 'disconnected') {
        response.destroy();
      } else if (mode === 'empty') {
        response.end();
      } else {
        response.writeHead(200, { 'Content-Length': '100' });
        response.flushHeaders();
        if (mode === 'truncated') {
          const socket = response.socket;
          response.end('partial');
          setImmediate(() => socket.destroy());
        }
      }
    });
    await assert.rejects(downloadFile(url, destination, { timeoutMs: 150 }));
    assert.equal(await readFile(destination, 'utf8'), 'usable previous binary');
    assert.deepEqual(await readdir(dir), ['binary']);
  });
}

test('failed executable validation does not replace a usable binary', async (t) => {
  const dir = await directory(t);
  const destination = join(dir, 'binary');
  await writeFile(destination, 'usable previous binary');
  const url = await server(t, (_request, response) => response.end('<html>not a binary</html>'));
  await assert.rejects(downloadFile(url, destination, {
    validate() { throw new Error('invalid executable'); },
  }), /invalid executable/);
  assert.equal(await readFile(destination, 'utf8'), 'usable previous binary');
  assert.deepEqual(await readdir(dir), ['binary']);
});

test('installer recovers from an empty cache and reuses only a verified binary', {
  skip: process.platform === 'win32',
}, async (t) => {
  const dir = await directory(t);
  await mkdir(join(dir, 'scripts'));
  await mkdir(join(dir, 'bin'));
  for (const name of ['postinstall.js', 'download.js']) {
    await copyFile(join(root, 'scripts', name), join(dir, 'scripts', name));
  }
  await writeFile(join(dir, 'package.json'), '{"type":"module","version":"0.41.2"}');
  const binary = join(dir, 'bin', 'agent-browser-linux-x64');
  await writeFile(binary, '');
  let mode = 'missing';
  let requests = 0;
  const url = await server(t, (_request, response) => {
    requests++;
    if (mode === 'missing') {
      response.writeHead(404);
      response.end();
    } else {
      response.end(mode === 'invalid' ? '<html>not executable</html>' :
        '#!/bin/sh\nprintf "agent-browser 0.41.2\\n"\n');
    }
  });
  const bootstrap = join(dir, 'bootstrap.mjs');
  await writeFile(bootstrap, `
import { createRequire, syncBuiltinESMExports } from 'node:module';
const require = createRequire(import.meta.url);
const os = require('node:os');
const childProcess = require('node:child_process');
const https = require('node:https');
const http = require('node:http');
os.platform = () => 'linux';
os.arch = () => 'x64';
const execSync = childProcess.execSync;
childProcess.execSync = (command, options) => command.startsWith('ldd ') ? 'glibc' : execSync(command, options);
https.get = (_url, options, callback) => http.get(${JSON.stringify(url)}, options, callback);
syncBuiltinESMExports();
await import('./scripts/postinstall.js');
`);

  async function install() {
    const child = spawn(process.execPath, [bootstrap], {
      env: { ...process.env, npm_config_global: 'false', npm_config_user_agent: 'npm/10.0.0' },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.on('data', (chunk) => { output += chunk; });
    child.stderr.on('data', (chunk) => { output += chunk; });
    const code = await new Promise((resolve, reject) => {
      child.on('error', reject);
      child.on('close', resolve);
    });
    return { code, output };
  }

  let result = await install();
  assert.equal(result.code, 1);
  assert.match(result.output, /Could not install native binary/);
  assert.doesNotMatch(result.output, /Native binary ready|Downloaded native binary/);
  mode = 'invalid';
  result = await install();
  assert.equal(result.code, 1);
  assert.equal((await stat(binary)).size, 0);
  mode = 'valid';
  result = await install();
  assert.equal(result.code, 0, result.output);
  assert.match(result.output, /Downloaded native binary/);
  assert.ok((await stat(binary)).size > 0);
  mode = 'missing';
  result = await install();
  assert.equal(result.code, 0, result.output);
  assert.match(result.output, /Native binary ready/);
  assert.equal(requests, 3, 'verified cache must avoid another network request');
  assert.ok((await readdir(join(dir, 'bin'))).every((name) => !name.includes('.download')));
});
