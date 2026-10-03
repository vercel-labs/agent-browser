import { Sandbox } from '@vercel/sandbox';
import { credentials as projectCredentials, loadLocalEnv } from './auth.mjs';
import { collectGuestArtifacts, executeGuest, finalizeTrialResult, stopSandbox } from './lifecycle.mjs';
import { CHROMIUM_SYSTEM_DEPS } from '@agent-browser/sandbox/vercel';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { parseArgs } from 'node:util';
import { ROOT, REMOTE, CASES, sourceFiles, fingerprint, matrix, runtimeEnv, gatewayPolicy, comparisons } from './config.mjs';

const { values: options, positionals } = parseArgs({ allowPositionals: true, options: {
  provider: { type: 'string', default: 'both' }, mode: { type: 'string', default: 'paired' },
  'browser-mode': { type: 'string', default: 'both' }, case: { type: 'string', multiple: true },
  runs: { type: 'string', default: '1' }, timeout: { type: 'string', default: '180' },
  results: { type: 'string' }, snapshot: { type: 'string', default: resolve(ROOT, 'evals/.sandbox-snapshot.json') },
  permissions: { type: 'string', default: 'unattended' }, 'claude-model': { type: 'string' }, 'codex-model': { type: 'string' },
  help: { type: 'boolean' }, list: { type: 'boolean' },
} });

const action = positionals[0] ?? 'run';
if (options.help) {
  console.log(`Usage: pnpm --dir evals run eval:live [options]
       pnpm --dir evals run sandbox:prepare [options]

Build the checkout once, then run each trial in a fresh Vercel Sandbox.
  --provider claude|codex|both       Default: both
  --mode interactive|headless|paired  CLI transport; default: paired
  --browser-mode headed|headless|both Default: both (headed uses Xvfb)
  --case NAME                       Repeat to select cases; --list lists them
  --runs N                          Repetitions; default: 1
  --timeout SECONDS                 Per-case model timeout; default: 180
  --permissions unattended|default  Provider policy inside VM; default: unattended
  --claude-model MODEL              Override the pinned default model
  --codex-model MODEL               Override the pinned default model
  --results DIRECTORY               New local artifact directory
  --snapshot FILE                   Snapshot manifest from sandbox:prepare
Auth: link evals/ with Vercel and run vercel env pull, or set VERCEL_OIDC_TOKEN.
AI Gateway uses OIDC via Sandbox credential brokering. No API key is needed.
Preparing a snapshot never receives model credentials. Expired snapshots must be rebuilt.`);
  process.exit(0);
}
if (options.list) { console.log(CASES.join('\n')); process.exit(0); }
for (const [key, choices] of Object.entries({ provider: ['claude', 'codex', 'both'], mode: ['interactive', 'headless', 'paired'],
  'browser-mode': ['headed', 'headless', 'both'], permissions: ['unattended', 'default'] })) {
  if (!choices.includes(options[key])) throw new Error(`Invalid --${key}: ${options[key]}`);
}
if (!['run', 'prepare'].includes(action) || positionals.length > 1) throw new Error('Expected run or prepare');
if (!Number.isInteger(Number(options.runs)) || Number(options.runs) < 1) throw new Error('--runs must be a positive integer');
if (!Number.isFinite(Number(options.timeout)) || Number(options.timeout) <= 0 || Number(options.timeout) > 1800) throw new Error('--timeout must be between 0 and 1800 seconds');
if (options.case?.some(c => !CASES.includes(c))) throw new Error('Unknown --case; use --list');

const json = async (path, value) => { await mkdir(dirname(path), { recursive: true }); await writeFile(path, JSON.stringify(value, null, 2) + '\n'); };
const pins = JSON.parse(await readFile(resolve(ROOT, 'evals/sandbox/environment.json'), 'utf8'));
let active;
const abort = new AbortController();
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
  console.error(`${signal}: cancelling; downloading available artifacts and stopping the sandbox.`);
  abort.abort();
});

async function credentials() {
  return projectCredentials(action === 'prepare' ? 24 * 60_000 : (Number(options.timeout) + 240) * 1000);
}

async function command(sandbox, cmd, args = [], extra = {}) {
  const result = await sandbox.runCommand({ cmd, args, stdout: process.stdout, stderr: process.stderr,
    signal: abort.signal, ...extra });
  if (result.exitCode !== 0) throw new Error(`${cmd} exited with ${result.exitCode}`);
  return result;
}

async function upload(sandbox, files) {
  // Bound upload batches; the CDP protocol and embedded assets can be large.
  for (let i = 0; i < files.length; i += 40) await sandbox.writeFiles(files.slice(i, i + 40), { signal: abort.signal });
}

async function guestFiles() {
  const paths = ['evals/live-evals.py', 'evals/sandbox/guest.py'];
  for (const name of (await readdir(resolve(ROOT, 'evals/live'))).sort()) {
    if (name.endsWith('.py') && !name.startsWith('test_')) paths.push(`evals/live/${name}`);
  }
  return Promise.all(paths.map(async path => ({ path: `${REMOTE}/repo/${path}`, content: await readFile(resolve(ROOT, path)) })));
}

async function prepare(auth, files, sourceHash) {
  console.log('Preparing a credential-free eval snapshot. Building the current checkout.');
  const sandbox = active = await Sandbox.create({ ...auth, runtime: pins.runtime, persistent: false,
    resources: { vcpus: pins.vcpus }, timeout: 20 * 60_000, signal: abort.signal });
  try {
    console.log(`Build sandbox: ${sandbox.name}`);
    await upload(sandbox, files);
    await sandbox.writeFiles([{ path: `${REMOTE}/system-deps.json`, content: JSON.stringify(CHROMIUM_SYSTEM_DEPS) }]);
    await command(sandbox, 'python3', [`${REMOTE}/repo/evals/sandbox/bootstrap.py`], { timeoutMs: 18 * 60_000 });
    const environment = JSON.parse((await sandbox.readFileToBuffer({ path: `${REMOTE}/environment.json` })).toString());
    const metadata = { schema: 1, created: new Date().toISOString(), sourceHash,
      projectId: auth.projectId, teamId: auth.teamId, environment };
    await sandbox.writeFiles([{ path: `${REMOTE}/snapshot-manifest.json`, content: JSON.stringify(metadata) }]);
    const snapshot = await sandbox.snapshot({ expiration: 30 * 24 * 60 * 60_000, signal: abort.signal });
    await json(resolve(options.snapshot), { ...metadata, snapshotId: snapshot.snapshotId });
    console.log(`Snapshot: ${snapshot.snapshotId}\nManifest: ${resolve(options.snapshot)}`);
  } finally {
    await sandbox.stop();
    active = undefined;
  }
}

async function runTrial(auth, snapshot, row, folder, guest) {
  await mkdir(folder);
  const sandbox = active = await Sandbox.create({ ...auth, source: { type: 'snapshot', snapshotId: snapshot.snapshotId },
    persistent: false, resources: { vcpus: pins.vcpus }, timeout: (Number(options.timeout) + 180) * 1000,
    networkPolicy: gatewayPolicy(auth.token),
    signal: abort.signal });
  let exitCode;
  let error;
  let cleanupError;
  const provenance = { ...row, sandboxName: sandbox.name, snapshotId: snapshot.snapshotId,
    sourceHash: snapshot.sourceHash, runnerHash: fingerprint(guest), permissions: options.permissions,
    auth: 'oidc-credential-brokering', projectId: auth.projectId, teamId: auth.teamId };
  await json(resolve(folder, 'sandbox.json'), provenance);
  console.log(`\n${row.provider} ${row.mode} / browser ${row.browser} / ${row.case} / trial ${row.trial}\nSandbox: ${sandbox.name}`);
  console.log(`Watch with the Vercel Sandbox CLI: sandbox exec -it ${sandbox.name} -- sh\nThen attach using the tmux command printed below.`);
  try {
    const remoteMetadata = JSON.parse((await sandbox.readFileToBuffer({ path: `${REMOTE}/snapshot-manifest.json` })).toString());
    if (remoteMetadata.sourceHash !== snapshot.sourceHash) throw new Error('Snapshot provenance does not match the local manifest');
    await upload(sandbox, guest);
    await sandbox.writeFiles([{ path: `${REMOTE}/trial.json`, content: JSON.stringify({ ...row,
      timeout: Number(options.timeout), permissions: options.permissions,
      claudeModel: options['claude-model'] ?? pins.claudeModel, codexModel: options['codex-model'] ?? pins.codexModel }) }]);
    const result = await executeGuest(sandbox, { cmd: 'python3.11', args: [`${REMOTE}/repo/evals/sandbox/guest.py`],
      env: runtimeEnv(row.provider), stdout: process.stdout, stderr: process.stderr,
      timeoutMs: (Number(options.timeout) + 90) * 1000 }, abort.signal);
    exitCode = result.exitCode;
  } catch (e) {
    error = String(e);
  } finally {
    // This finally also runs on failure/interrupt. No credentials or home profiles are archived.
    console.log('  Collecting trial artifacts.');
    const cleanupErrors = await collectGuestArtifacts(sandbox, { folder, remote: REMOTE });
    console.log('  Stopping sandbox.');
    const stopError = await stopSandbox(sandbox);
    if (stopError) cleanupErrors.push(`Sandbox stop failed: ${stopError}`);
    active = undefined;
    if (cleanupErrors.length) cleanupError = cleanupErrors.join('; ');
  }
  let result;
  try { result = JSON.parse(await readFile(resolve(folder, 'results.json'), 'utf8')).results[0]; } catch { /* Fail closed below. */ }
  const final = finalizeTrialResult({ result, provenance, exitCode, executionError: error, cleanupError });
  await json(resolve(folder, 'result.json'), final);
  return final;
}

async function main() {
  // pnpm runs here from evals/, but direct node invocations should use the same link.
  process.chdir(resolve(ROOT, 'evals'));
  loadLocalEnv();
  const auth = await credentials();
  const files = await sourceFiles();
  const sourceHash = fingerprint(files);
  if (action === 'prepare') return prepare(auth, files, sourceHash);
  const snapshot = JSON.parse(await readFile(resolve(options.snapshot), 'utf8').catch(() => {
    throw new Error('Prepare the environment first: pnpm --dir evals run sandbox:prepare');
  }));
  if (snapshot.sourceHash !== sourceHash) throw new Error('Checkout or environment changed. Run sandbox:prepare again to build a matching snapshot.');
  if (snapshot.projectId !== auth.projectId || snapshot.teamId !== auth.teamId) throw new Error('Snapshot belongs to a different Vercel project. Run sandbox:prepare in the linked eval project.');
  const output = resolve(options.results ?? resolve(ROOT, 'evals/results', `sandbox-${new Date().toISOString().replaceAll(':', '-')}`));
  await mkdir(dirname(output), { recursive: true });
  await mkdir(output);
  const guest = await guestFiles();
  await json(resolve(output, 'environment.json'), snapshot);
  const results = [];
  try {
    for (const row of matrix({ provider: options.provider, mode: options.mode, browser: options['browser-mode'],
      cases: options.case, runs: Number(options.runs) })) {
      if (abort.signal.aborted) break;
      const name = `${row.provider}-${row.mode}-${row.browser}-${row.case}-${row.trial}`;
      results.push(await runTrial(await credentials(), snapshot, row, resolve(output, name), guest));
      await json(resolve(output, 'results.json'), { results, comparisons: comparisons(results) });
    }
  } finally {
    if (active) {
      const stopError = await stopSandbox(active);
      active = undefined;
      if (stopError) console.error(`Sandbox cleanup failed: ${stopError}`);
    }
  }
  console.log(`\nPassed ${results.filter(r => r.passed).length}/${results.length}. Report: ${output}/results.json`);
  process.exitCode = abort.signal.aborted ? 130 : results.every(r => r.passed) ? 0 : 1;
}

main().catch(error => { console.error(error.message); process.exitCode = 1; });
