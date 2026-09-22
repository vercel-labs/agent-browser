import { getVercelOidcToken } from '@vercel/oidc';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { ROOT } from './config.mjs';

/** Resolve refreshed project OIDC on the host; never serialize this result. */
export async function credentials(minimumValidityMs = 300_000) {
  let project;
  try { project = JSON.parse(await readFile(resolve(ROOT, 'evals/.vercel/project.json'), 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  const token = await getVercelOidcToken({ team: project?.orgId, project: project?.projectId,
    expirationBufferMs: minimumValidityMs });
  const claims = JSON.parse(Buffer.from(token.split('.')[1], 'base64url').toString());
  if (!claims.owner_id || !claims.project_id) throw new Error('OIDC token is missing its Vercel project scope');
  if (project && (claims.owner_id !== project.orgId || claims.project_id !== project.projectId)) {
    throw new Error('OIDC token does not match the project linked in evals/.vercel/project.json');
  }
  if (claims.exp * 1000 <= Date.now() + minimumValidityMs) throw new Error('OIDC token will expire before the trial ends. Refresh with vercel env pull.');
  return { token, teamId: claims.owner_id, projectId: claims.project_id };
}

export function loadLocalEnv() {
  try { process.loadEnvFile(resolve(ROOT, 'evals/.env.local')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
}
