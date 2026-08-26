import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, existsSync, readFileSync, rmSync } from 'fs';
import { join, dirname } from 'path';
import { tmpdir } from 'os';
import { createHash } from 'crypto';
import { fileURLToPath } from 'url';
import { verifyChecksum } from './verify-checksum.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const VERSION = '9.9.9-test';
const BINARY = 'agent-browser-linux-x64';
const silent = { warn: () => {} };

// Write a temp checksums.json recording the sha256 of `realBytes` for BINARY@VERSION.
function setup(realBytes) {
  const dir = mkdtempSync(join(tmpdir(), 'ab-checksum-'));
  const sha = createHash('sha256').update(realBytes).digest('hex');
  const checksumsPath = join(dir, 'checksums.json');
  writeFileSync(checksumsPath, JSON.stringify({ [VERSION]: { [BINARY]: sha } }));
  return { dir, checksumsPath };
}

test('rejects a tampered binary and deletes it', () => {
  const { dir, checksumsPath } = setup(Buffer.from('the real signed binary'));
  const binPath = join(dir, BINARY);
  writeFileSync(binPath, Buffer.from('MALICIOUS payload, not the real binary'));

  assert.throws(
    () => verifyChecksum(binPath, BINARY, VERSION, { checksumsPath, log: silent }),
    (err) => err.code === 'ERR_CHECKSUM_MISMATCH'
  );
  assert.equal(existsSync(binPath), false, 'the tampered binary must be removed, not left executable');

  rmSync(dir, { recursive: true, force: true });
});

test('accepts a binary whose checksum matches', () => {
  const good = Buffer.from('the real signed binary');
  const { dir, checksumsPath } = setup(good);
  const binPath = join(dir, BINARY);
  writeFileSync(binPath, good);

  const result = verifyChecksum(binPath, BINARY, VERSION, { checksumsPath, log: silent });
  assert.equal(result, true);
  assert.equal(existsSync(binPath), true, 'a valid binary must be left in place');

  rmSync(dir, { recursive: true, force: true });
});

test('skips (warns, does not throw) when no checksum is recorded', () => {
  const { dir, checksumsPath } = setup(Buffer.from('whatever'));
  const binPath = join(dir, BINARY);
  writeFileSync(binPath, Buffer.from('unverifiable but allowed'));

  let warned = false;
  const result = verifyChecksum(binPath, 'agent-browser-unknown-plat', '0.0.0-absent', {
    checksumsPath,
    log: { warn: () => { warned = true; } },
  });
  assert.equal(result, false, 'a missing checksum is skipped, not enforced');
  assert.equal(warned, true, 'the skip must be surfaced as a warning');
  assert.equal(existsSync(binPath), true, 'the install proceeds when no checksum is recorded');

  rmSync(dir, { recursive: true, force: true });
});

// Guard against placeholder/fake hashes ever shipping. Kept decoupled from the
// exact package.json version on purpose: checksums for a new version are only
// regenerated in the release pipeline, so coupling to the current version would
// make this fail in the window between a version bump and the release.
test('checksums.json holds only real sha256 values (no placeholders)', () => {
  const all = JSON.parse(readFileSync(join(__dirname, 'checksums.json'), 'utf8'));
  const versions = Object.keys(all);
  assert.ok(versions.length >= 1, 'checksums.json should record at least one version');
  let count = 0;
  for (const version of versions) {
    for (const [name, hash] of Object.entries(all[version])) {
      assert.match(hash, /^[a-f0-9]{64}$/, `${version}/${name} must be a real 64-char sha256, not a placeholder`);
      count++;
    }
  }
  assert.ok(count >= 1, 'expected at least one recorded checksum');
});
