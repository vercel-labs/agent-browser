import test from 'node:test';
import assert from 'node:assert/strict';
import { matrix, runtimeEnv, gatewayPolicy, BROKER_PLACEHOLDER, comparisons, fingerprint, sourceFiles } from './config.mjs';

test('pairs every CLI/browser combination and alternates CLI order across trials', () => {
  const rows = matrix({ runs: 2, cases: ['page-screenshot'] });
  assert.equal(rows.length, 16);
  assert.equal(new Set(rows.map(r => JSON.stringify(r))).size, 16);
  assert.equal(rows[0].mode, 'interactive');
  assert.equal(rows[8].mode, 'headless');
});

test('a missing mode cannot accidentally pair with another browser or trial', () => {
  const rows = [
    { provider: 'claude', mode: 'interactive', browser: 'headed', trial: 1, case: 'a', passed: true },
    { provider: 'claude', mode: 'headless', browser: 'headless', trial: 1, case: 'a', passed: false },
    { provider: 'claude', mode: 'headless', browser: 'headed', trial: 2, case: 'a', passed: false },
  ];
  assert.deepEqual(comparisons(rows), []);
  rows.push({ ...rows[0], mode: 'headless', passed: false });
  assert.equal(comparisons(rows).length, 1);
  assert.equal(comparisons(rows)[0].different_outcome, true);
});

test('guest credentials are placeholders and cannot contain host secrets', () => {
  const claude = runtimeEnv('claude');
  assert.equal(claude.ANTHROPIC_AUTH_TOKEN, BROKER_PLACEHOLDER);
  assert.equal(claude.DISABLE_AUTOUPDATER, '1');
  assert.deepEqual(runtimeEnv('codex'), { AI_GATEWAY_API_KEY: BROKER_PLACEHOLDER });
  for (const key of ['HOME', 'CODEX_HOME', 'VERCEL_TOKEN', 'VERCEL_OIDC_TOKEN']) assert.equal(claude[key], undefined);
});

test('OIDC is brokered only for placeholder-authenticated AI Gateway API requests', () => {
  const policy = gatewayPolicy('test-oidc-secret');
  const rules = policy.allow['ai-gateway.vercel.sh'];
  assert.deepEqual(Object.keys(policy.allow), ['ai-gateway.vercel.sh', '*']);
  assert.deepEqual(policy.allow['*'], []);
  assert.equal(rules[0].match.path.startsWith, '/v1/');
  assert.equal(rules[0].match.headers[0].value.exact, `Bearer ${BROKER_PLACEHOLDER}`);
  assert.equal(rules[0].transform[0].headers.authorization, 'Bearer test-oidc-secret');
  assert(!JSON.stringify(runtimeEnv('claude')).includes('test-oidc-secret'));
  assert.throws(() => gatewayPolicy(''), /OIDC/);
});

test('source fingerprints detect edits and renames', () => {
  const a = [{ path: 'cli/a.rs', content: Buffer.from('one') }];
  assert.notEqual(fingerprint(a), fingerprint([{ ...a[0], content: Buffer.from('two') }]));
  assert.notEqual(fingerprint(a), fingerprint([{ ...a[0], path: 'cli/b.rs' }]));
});

test('snapshot uploads contain source and locks but no local credentials or build caches', async () => {
  const paths = (await sourceFiles()).map(f => f.path);
  assert(paths.some(p => p.endsWith('/cli/Cargo.lock')));
  assert(paths.some(p => p.endsWith('/repo/evals/package.json')));
  assert(paths.some(p => p.endsWith('/repo/evals/pnpm-lock.yaml')));
  assert(paths.some(p => p.endsWith('/tools/pnpm-lock.yaml')));
  assert(!paths.some(p => /\/(node_modules|target|results|\.git|\.claude|\.codex)\//.test(p)));
  assert(!paths.some(p => /\/\.env/.test(p)));
});
