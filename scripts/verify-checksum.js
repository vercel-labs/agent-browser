/**
 * SHA-256 integrity verification for the downloaded native binary.
 *
 * Kept in its own module (no side effects on import) so postinstall.js can call
 * it and the regression test can exercise it directly.
 *
 * Behaviour:
 *   - A checksum is recorded for this version+binary -> verify, throw on mismatch.
 *   - No checksum is recorded -> skip with a warning (do not block the install).
 * This "verify when present" gate keeps installs working across version bumps
 * (a checksums.json that hasn't been regenerated for a new version won't falsely
 * reject it) while still catching a tampered/corrupted download when we do have a
 * known-good value to compare against.
 */
import { existsSync, readFileSync, unlinkSync } from 'fs';
import { createHash } from 'crypto';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DEFAULT_CHECKSUMS_PATH = join(__dirname, 'checksums.json');

/**
 * Return the recorded sha256 for a binary at a given version, or null if none.
 */
export function loadExpectedChecksum(binaryName, version, checksumsPath = DEFAULT_CHECKSUMS_PATH) {
  if (!existsSync(checksumsPath)) return null;
  try {
    const all = JSON.parse(readFileSync(checksumsPath, 'utf8'));
    const forVersion = all[version];
    return (forVersion && forVersion[binaryName]) || null;
  } catch {
    return null;
  }
}

/**
 * Verify the file at `filePath` against the recorded checksum for
 * `binaryName` at `version`.
 *
 * @returns {boolean} true if verified, false if verification was skipped
 *   (no recorded checksum).
 * @throws {Error} with code 'ERR_CHECKSUM_MISMATCH' if the file's sha256 does
 *   not match the recorded value. The mismatched file is deleted before throwing.
 */
export function verifyChecksum(filePath, binaryName, version, opts = {}) {
  const { checksumsPath, log = console } = opts;
  const expected = loadExpectedChecksum(binaryName, version, checksumsPath);

  if (!expected) {
    log.warn(`⚠ Skipping integrity check: no recorded checksum for ${binaryName} at v${version}.`);
    return false;
  }

  const actual = createHash('sha256').update(readFileSync(filePath)).digest('hex');
  if (actual !== expected) {
    try {
      unlinkSync(filePath);
    } catch {
      // best-effort cleanup; the throw below is what matters
    }
    const err = new Error(
      `Checksum mismatch for ${binaryName} (v${version}).\n` +
        `  expected: ${expected}\n` +
        `  actual:   ${actual}\n` +
        `The downloaded binary does not match the checksum shipped with this package, ` +
        `so it was deleted and NOT installed. This usually means a corrupted download ` +
        `or a tampered release artifact.`
    );
    err.code = 'ERR_CHECKSUM_MISMATCH';
    throw err;
  }

  return true;
}
