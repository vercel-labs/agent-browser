import test from 'node:test';
import assert from 'node:assert/strict';
import { collectGuestArtifacts, executeGuest, finalizeTrialResult, stopSandbox } from './lifecycle.mjs';

test('cancellation waits for guest cleanup before artifact collection can start', async () => {
  const events = [];
  let attempts = 0;
  const command = {
    async wait() { events.push('wait'); if (++attempts === 1) throw new Error('cancelled'); return { exitCode: 130 }; },
    async kill(signal) { events.push(signal); },
  };
  await assert.rejects(executeGuest({ async runCommand() { return command; } }, {}, new AbortController().signal), /cancelled/);
  assert.deepEqual(events, ['wait', 'SIGTERM', 'wait']);
});

test('unresponsive cleanup is bounded and force-killed', async () => {
  const events = [];
  const command = { async wait() { throw new Error('timeout'); }, async kill(signal) { events.push(signal); } };
  await assert.rejects(executeGuest({ async runCommand() { return command; } }, {}, new AbortController().signal), /timeout/);
  assert.deepEqual(events, ['SIGTERM', 'SIGKILL']);
});

test('a missing guest report does not suppress available artifacts', async () => {
  const events = [];
  const sandbox = {
    async readFileToBuffer() { events.push('report'); throw new Error('report missing'); },
    async runCommand() { events.push('archive'); return { exitCode: 0 }; },
    async downloadFile() { events.push('download'); return true; },
  };
  const errors = await collectGuestArtifacts(sandbox, { folder: '/tmp/eval-results', remote: '/vercel/sandbox' });
  assert.deepEqual(events, ['report', 'archive', 'download']);
  assert.equal(errors.length, 1);
  assert.match(errors[0], /Guest report collection failed.*report missing/);
});

test('sandbox stop failures are returned instead of replacing the trial result', async () => {
  const error = await stopSandbox({ async stop() { throw new Error('stop unavailable'); } });
  assert.match(error, /stop unavailable/);
});

test('cleanup failures preserve guest errors and fail the final trial result', () => {
  const result = finalizeTrialResult({ result: { passed: false, error: 'guest timed out' }, provenance: {},
    exitCode: 1, cleanupError: 'sandbox stop failed' });
  assert.equal(result.passed, false);
  assert.equal(result.error, 'guest timed out; sandbox stop failed');
});

test('cleanup failures turn an otherwise passing trial into a failure', () => {
  const result = finalizeTrialResult({ result: { passed: true, error: null }, provenance: {},
    exitCode: 0, cleanupError: 'artifact download failed' });
  assert.equal(result.passed, false);
  assert.equal(result.error, 'artifact download failed');
});
