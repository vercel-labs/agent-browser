import { createHash } from 'node:crypto';
import { readFile, readdir, lstat } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const ROOT = resolve(fileURLToPath(new URL('../..', import.meta.url)));
export const REMOTE = '/vercel/sandbox';
export const CASES = ['page-screenshot', 'form-submit', 'local-doc-edit', 'local-code-fix'];
export const digest = (data) => createHash('sha256').update(data).digest('hex');

// An allowlist prevents uploading logins, local results, .env files or build caches.
export async function sourceFiles() {
  const files = [];
  async function visit(path) {
    const stat = await lstat(resolve(ROOT, path));
    if (stat.isSymbolicLink()) throw new Error(`Refusing source symlink: ${path}`);
    if (stat.isDirectory()) {
      for (const entry of (await readdir(resolve(ROOT, path))).sort()) await visit(`${path}/${entry}`);
    } else {
      files.push({ path: `${REMOTE}/repo/${path}`, content: await readFile(resolve(ROOT, path)) });
    }
  }
  for (const path of ['cli/src', 'cli/cdp-protocol', 'cli/Cargo.toml', 'cli/Cargo.lock', 'cli/build.rs',
    'README.md', 'skills/agent-browser/SKILL.md', 'skill-data', 'evals/package.json', 'evals/pnpm-lock.yaml',
    'evals/sandbox/environment.json', 'evals/sandbox/bootstrap.py', 'evals/sandbox/tools/package.json',
    'evals/sandbox/tools/pnpm-lock.yaml']) await visit(path);
  return files;
}

export function fingerprint(files) {
  return digest(Buffer.concat(files.map(({ path, content }) => Buffer.concat([Buffer.from(path + '\0'), Buffer.from(digest(content) + '\n')]))));
}

export function matrix({ provider = 'both', mode = 'paired', browser = 'both', cases = CASES, runs = 1 }) {
  const rows = [];
  for (let trial = 1; trial <= runs; trial++) {
    for (const p of provider === 'both' ? ['claude', 'codex'] : [provider]) {
      for (const c of cases) {
        for (const b of browser === 'both' ? ['headless', 'headed'] : [browser]) {
          const modes = mode === 'paired' ? ['interactive', 'headless'] : [mode];
          for (const m of trial % 2 ? modes : modes.toReversed()) rows.push({ provider: p, mode: m, browser: b, case: c, trial });
        }
      }
    }
  }
  return rows;
}

export const BROKER_PLACEHOLDER = 'sandbox-brokered-oidc';

export function gatewayPolicy(token) {
  if (!token) throw new Error('An OIDC token is required for AI Gateway credential brokering.');
  return { allow: {
    'ai-gateway.vercel.sh': [{
      match: { path: { startsWith: '/v1/' }, headers: [
        { key: { exact: 'authorization' }, value: { exact: `Bearer ${BROKER_PLACEHOLDER}` } },
      ] },
      transform: [{ headers: { authorization: `Bearer ${token}` } }],
    }],
    '*': [],
  } };
}

export function runtimeEnv(provider) {
  // Only a non-secret placeholder enters the guest. The firewall supplies OIDC.
  return provider === 'claude' ? {
    ANTHROPIC_AUTH_TOKEN: BROKER_PLACEHOLDER,
    ANTHROPIC_BASE_URL: 'https://ai-gateway.vercel.sh',
    DISABLE_AUTOUPDATER: '1', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1',
  } : { AI_GATEWAY_API_KEY: BROKER_PLACEHOLDER };
}

export function comparisons(results) {
  const groups = new Map();
  for (const r of results) {
    const key = JSON.stringify([r.provider, r.case, r.trial, r.browser]);
    groups.set(key, { ...groups.get(key), [r.mode]: r });
  }
  return [...groups.values()].filter(g => g.interactive && g.headless).map(g => ({
    provider: g.interactive.provider, case: g.interactive.case, trial: g.interactive.trial,
    browser: g.interactive.browser, interactive_passed: g.interactive.passed, headless_passed: g.headless.passed,
    different_outcome: g.interactive.passed !== g.headless.passed,
  }));
}
