#!/usr/bin/env node

/**
 * Verifies that package.json and cli/Cargo.toml have the same version.
 * Used in CI to catch version drift.
 */

import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '..');

// Read package.json version
const packageJson = JSON.parse(readFileSync(join(rootDir, 'package.json'), 'utf-8'));
const packageVersion = packageJson.version;

// Read Cargo.toml version
const cargoToml = readFileSync(join(rootDir, 'cli/Cargo.toml'), 'utf-8');
const cargoVersionMatch = cargoToml.match(/^version\s*=\s*"([^"]*)"/m);

if (!cargoVersionMatch) {
  console.error('Could not find version in cli/Cargo.toml');
  process.exit(1);
}

const cargoVersion = cargoVersionMatch[1];

// Read dashboard package.json version
const dashboardPkg = JSON.parse(readFileSync(join(rootDir, 'packages/dashboard/package.json'), 'utf-8'));
const dashboardVersion = dashboardPkg.version;

// Read sandbox package versions
const sandboxPkg = JSON.parse(readFileSync(join(rootDir, 'packages/@agent-browser/sandbox/package.json'), 'utf-8'));
const sandboxVersion = sandboxPkg.version;
const sandboxVersionSource = readFileSync(join(rootDir, 'packages/@agent-browser/sandbox/src/version.ts'), 'utf-8');
const sandboxVersionMatch = sandboxVersionSource.match(/AGENT_BROWSER_SANDBOX_VERSION\s*=\s*"([^"]*)"/);

if (!sandboxVersionMatch) {
  console.error('Could not find AGENT_BROWSER_SANDBOX_VERSION in packages/@agent-browser/sandbox/src/version.ts');
  process.exit(1);
}

const sandboxRuntimeVersion = sandboxVersionMatch[1];

const mismatches = [];
if (packageVersion !== cargoVersion) {
  mismatches.push(`  cli/Cargo.toml:              ${cargoVersion}`);
}
if (packageVersion !== dashboardVersion) {
  mismatches.push(`  packages/dashboard:          ${dashboardVersion}`);
}
if (packageVersion !== sandboxVersion) {
  mismatches.push(`  packages/@agent-browser/sandbox/package.json: ${sandboxVersion}`);
}
if (packageVersion !== sandboxRuntimeVersion) {
  mismatches.push(`  packages/@agent-browser/sandbox/src/version.ts: ${sandboxRuntimeVersion}`);
}

if (mismatches.length > 0) {
  console.error('Version mismatch detected!');
  console.error(`  package.json:                ${packageVersion}`);
  for (const m of mismatches) console.error(m);
  console.error('');
  console.error("Run 'pnpm run version:sync' to fix this.");
  process.exit(1);
}

console.log(`Versions are in sync: ${packageVersion}`);
