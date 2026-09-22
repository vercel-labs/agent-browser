import { writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';

/** Stop the guest gracefully before collecting its artifacts on cancellation. */
export async function executeGuest(sandbox, parameters, signal) {
  const command = await sandbox.runCommand({ ...parameters, detached: true, signal });
  try {
    return await command.wait({ signal });
  } catch (error) {
    try {
      await command.kill('SIGTERM');
      await command.wait({ signal: AbortSignal.timeout(45_000) });
    } catch {
      try { await command.kill('SIGKILL'); } catch { /* VM shutdown is the final bound. */ }
    }
    throw error;
  }
}

/** Collect each available diagnostic even when the guest report is missing. */
export async function collectGuestArtifacts(sandbox, { folder, remote }) {
  const errors = [];
  try {
    const report = await sandbox.readFileToBuffer({ path: `${remote}/results/results.json` });
    if (report) await writeFile(resolve(folder, 'results.json'), report);
  } catch (error) {
    errors.push(`Guest report collection failed: ${error}`);
  }
  try {
    const archive = await sandbox.runCommand({ cmd: 'tar', args: ['-czf', `${remote}/artifacts.tar.gz`,
      '--ignore-failed-read', '-C', remote, 'results', 'xvfb.log', 'environment.json', 'trial.json'], timeoutMs: 30_000 });
    if (archive.exitCode !== 0) throw new Error(`Artifact archive exited with ${archive.exitCode}`);
    if (!await sandbox.downloadFile({ path: `${remote}/artifacts.tar.gz` }, { path: resolve(folder, 'artifacts.tar.gz') })) {
      throw new Error('Artifact archive missing');
    }
  } catch (error) {
    errors.push(`Artifact archive collection failed: ${error}`);
  }
  return errors;
}

/** Return cleanup failures so callers can preserve their primary result. */
export async function stopSandbox(sandbox) {
  try {
    await sandbox.stop();
    return null;
  } catch (error) {
    return String(error);
  }
}

/** Preserve guest diagnostics while making infrastructure cleanup failures fatal. */
export function finalizeTrialResult({ result, provenance, exitCode, executionError, cleanupError }) {
  const exitError = exitCode === 0 ? null : `Guest exited with ${exitCode ?? 'unknown status'}`;
  const primaryError = executionError ?? result?.error ?? exitError;
  const error = cleanupError ? (primaryError ? `${primaryError}; ${cleanupError}` : cleanupError) : primaryError;
  const final = { ...result, ...provenance,
    passed: Boolean(result?.passed && exitCode === 0 && !executionError && !cleanupError), error, exitCode };
  if (!result) { final.error ??= 'Missing guest result'; final.passed = false; }
  return final;
}
