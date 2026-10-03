import { Sandbox } from '@vercel/sandbox';
import { credentials, loadLocalEnv } from './auth.mjs';
import { BROKER_PLACEHOLDER, gatewayPolicy, runtimeEnv } from './config.mjs';

loadLocalEnv();
const auth = await credentials();
const sandbox = await Sandbox.create({ ...auth, runtime: 'node24', persistent: false, timeout: 60_000,
  networkPolicy: gatewayPolicy(auth.token) });
try {
  // The guest receives only the placeholder. Check both provider API protocols.
  for (const [path, payload] of [
    ['/v1/messages', { model: 'anthropic/claude-sonnet-4.6', max_tokens: 8, messages: [{ role: 'user', content: 'Say OK' }] }],
    ['/v1/responses', { model: 'openai/gpt-5.4', max_output_tokens: 32, input: 'Say OK' }],
  ]) {
    const result = await sandbox.runCommand({ cmd: 'curl', args: ['--silent', '--show-error', '--fail-with-body',
      'https://ai-gateway.vercel.sh' + path, '-H', `Authorization: Bearer ${BROKER_PLACEHOLDER}`,
      '-H', 'Content-Type: application/json', '-H', 'anthropic-version: 2023-06-01', '-d', JSON.stringify(payload)],
      timeoutMs: 30_000 });
    const output = await result.stdout();
    if (result.exitCode !== 0) throw new Error(`Brokered ${path} failed: ${output || await result.stderr()}`);
    const body = JSON.parse(output);
    if (!body.id || body.error) throw new Error(`Brokered ${path} returned no model response`);
    console.log(`PASS ${path}: authenticated with brokered project OIDC`);
  }
  const env = await sandbox.runCommand({ cmd: 'printenv', env: runtimeEnv('codex') });
  const guestEnv = await env.stdout();
  if (guestEnv.includes(auth.token) || guestEnv.includes('VERCEL_OIDC_TOKEN=')) throw new Error('OIDC unexpectedly present in guest environment');
  console.log(`PASS guest environment contains no OIDC token; project ${auth.projectId}`);
} finally { await sandbox.stop(); }
