#!/usr/bin/env node
/**
 * Regenerate scripts/checksums.json from the built platform binaries in bin/.
 *
 * Run this in the release pipeline after the binaries have been placed in bin/
 * and before publishing, so the checksums shipped inside the package match the
 * exact artifacts uploaded to the GitHub release. Values are keyed by version.
 * Install-time verification only needs the running version's entry; any other
 * versions present are whatever is already committed in checksums.json (this
 * script merges rather than overwrites, but it does not commit its output back).
 */
import { readdirSync, readFileSync, writeFileSync, existsSync } from 'fs';
import { createHash } from 'crypto';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = join(__dirname, '..');
const binDir = join(projectRoot, 'bin');
const checksumsPath = join(__dirname, 'checksums.json');

const version = JSON.parse(readFileSync(join(projectRoot, 'package.json'), 'utf8')).version;

if (!existsSync(binDir)) {
  console.error(`No bin/ directory at ${binDir}; build the binaries first.`);
  process.exit(1);
}

const entries = {};
for (const name of readdirSync(binDir).sort()) {
  // Only the downloadable native binaries (agent-browser-<platform>); the JS
  // wrapper agent-browser.js and any dotfiles do not match this prefix.
  if (!name.startsWith('agent-browser-')) continue;
  entries[name] = createHash('sha256').update(readFileSync(join(binDir, name))).digest('hex');
}

if (Object.keys(entries).length === 0) {
  console.error('No agent-browser-* binaries found in bin/; nothing to checksum.');
  process.exit(1);
}

let all = {};
if (existsSync(checksumsPath)) {
  try {
    all = JSON.parse(readFileSync(checksumsPath, 'utf8'));
  } catch {
    all = {};
  }
}
all[version] = entries;
writeFileSync(checksumsPath, JSON.stringify(all, null, 2) + '\n');
console.log(`Wrote ${Object.keys(entries).length} checksum(s) for v${version} to ${checksumsPath}`);
