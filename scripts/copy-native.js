#!/usr/bin/env node

/**
 * Copies the compiled Rust binary to bin/ with platform-specific naming
 */

import { copyFileSync, existsSync, mkdirSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { platform, arch } from 'os';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = join(__dirname, '..');

const sourceExt = platform() === 'win32' ? '.exe' : '';
const sourcePath = join(projectRoot, `cli/target/release/agent-browser${sourceExt}`);
const binDir = join(projectRoot, 'bin');

// Determine platform suffix
const platformKey = `${platform()}-${arch()}`;
const ext = platform() === 'win32' ? '.exe' : '';
const targetName = `agent-browser-${platformKey}${ext}`;
const targetPath = join(binDir, targetName);

if (!existsSync(sourcePath)) {
  console.error(`Error: Native binary not found at ${sourcePath}`);
  console.error('Run "cargo build --release --manifest-path cli/Cargo.toml" first');
  process.exit(1);
}

if (!existsSync(binDir)) {
  mkdirSync(binDir, { recursive: true });
}

copyFileSync(sourcePath, targetPath);
console.log(`✓ Copied native binary to ${targetPath}`);
