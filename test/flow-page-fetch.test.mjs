import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../cli/src/native/flow/page.rs', import.meta.url), 'utf8');
const template = source.match(/format!\(\s*r#"([\s\S]*?)"#,/)[1];
const sendScripts = Object.fromEntries(['READ', 'WRITE'].map((kind) => [
  kind,
  source.match(new RegExp('const PAGE_FETCH_' + kind + ': &str =\\s*r#"([\\s\\S]*?)"#;'))[1],
]));

function pageScript(write) {
  const args = {
    url: JSON.stringify('https://api.example.test/items'),
    init: JSON.stringify({
      method: write ? 'POST' : 'GET',
      headers: { 'x-test': 'ok' },
      body: write ? '{"name":"item"}' : null,
      redirect: 'manual',
    }),
    send: sendScripts[write ? 'WRITE' : 'READ'],
    timeout_ms: '50',
  };
  // Apply Rust format!'s escaped braces and named arguments in one pass.
  return template.replace(/\{\{|\}\}|\{(\w+)\}/g, (match, name) =>
    match === '{{' ? '{' : match === '}}' ? '}' : args[name]);
}

function response(overrides = {}) {
  return {
    status: 200,
    type: 'basic',
    url: 'https://api.example.test/items',
    headers: { get: (name) => name === 'retry-after' ? '3' : null },
    text: async () => '{"ok":true}',
    ...overrides,
  };
}

function pendingUntilAborted(signal) {
  return new Promise((resolve, reject) => {
    signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
  });
}

function run(write, fetch) {
  const calls = [];
  const timers = new Map();
  let nextTimer = 0;
  const result = vm.runInNewContext(pageScript(write), {
    AbortController,
    Error,
    fetch: (url, init) => {
      calls.push({ url, init });
      return fetch(url, init, calls.length);
    },
    setTimeout: (callback, delay) => {
      const id = ++nextTimer;
      timers.set(id, { callback, delay });
      return id;
    },
    clearTimeout: (id) => timers.delete(id),
  });
  return {
    result,
    calls,
    timers,
    expire() {
      assert.equal(timers.size, 1, 'fetch must have a page-side deadline');
      const [timer] = timers.values();
      assert.equal(timer.delay, 50);
      timer.callback();
    },
  };
}

for (const write of [false, true]) {
  test((write ? 'write' : 'read') + ' aborts stalled response headers without retrying', async () => {
    const request = run(write, (url, init) => pendingUntilAborted(init.signal));
    request.expire();
    await assert.rejects(request.result, /the run's time is up/);
    assert.equal(request.calls.length, 1);
    assert.equal(request.calls[0].init.signal.aborted, true);
    assert.equal(request.timers.size, 0);
  });
}

test('the deadline remains active while consuming the response body', async () => {
  let readingBody = false;
  const request = run(false, (url, init) => response({
    text: () => {
      readingBody = true;
      return pendingUntilAborted(init.signal);
    },
  }));
  await Promise.resolve();
  assert.equal(readingBody, true);
  request.expire();
  await assert.rejects(request.result, /the run's time is up/);
  assert.equal(request.calls.length, 1);
  assert.equal(request.timers.size, 0);
});

test('a read can still retry a credential error before the deadline', async () => {
  const request = run(false, (url, init, count) => {
    if (count === 1) throw new TypeError('credentialed CORS request failed');
    return response();
  });
  const value = await request.result;
  assert.equal(value.text, '{"ok":true}');
  assert.equal(request.calls.length, 2);
  assert.equal(request.calls[0].init.credentials, 'include');
  assert.equal(request.calls[1].init.credentials, undefined);
  assert.equal(request.calls[0].init.signal, request.calls[1].init.signal);
  assert.equal(request.timers.size, 0);
});

test('a failed write is never retried and releases its timer', async () => {
  const request = run(true, () => { throw new TypeError('connection lost'); });
  await assert.rejects(request.result, /connection lost/);
  assert.equal(request.calls.length, 1);
  assert.equal(request.timers.size, 0);
});

test('successful requests preserve response metadata and release their timer', async () => {
  const request = run(false, () => response());
  const value = await request.result;
  assert.deepEqual(JSON.parse(JSON.stringify(value)), {
    status: 200,
    redirected: false,
    url: 'https://api.example.test/items',
    retryAfter: '3',
    text: '{"ok":true}',
  });
  assert.equal(request.calls[0].init.redirect, 'manual');
  assert.equal(request.timers.size, 0);
});

test('opaque redirects retain the existing redirect result', async () => {
  const request = run(true, () => response({
    type: 'opaqueredirect', status: 0, url: '', text: async () => '',
  }));
  const value = await request.result;
  assert.equal(value.status, 302);
  assert.equal(value.redirected, true);
  assert.equal(value.url, '');
  assert.equal(request.calls.length, 1);
  assert.equal(request.timers.size, 0);
});
