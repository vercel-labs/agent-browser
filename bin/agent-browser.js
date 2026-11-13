#!/usr/bin/env node

/**
 * Cross-platform CLI launcher for agent-browser
 *
 * Runs the native binary from the platform package (an optional dependency
 * such as @agent-browser/cli-linux-x64-gnu) or from this package's bin/.
 * On first use it also points the package manager's bin entry at the native
 * binary, so later invocations skip Node.js entirely, even when install
 * scripts were blocked.
 */

import { spawn } from 'child_process';
import { accessSync, chmodSync, constants } from 'fs';
import { dirname } from 'path';
import { fileURLToPath } from 'url';
import { platform, arch } from 'os';
import { findNativeBinary, nativeTarget } from '../scripts/native.js';

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));

async function main() {
  const target = nativeTarget();

  if (!target) {
    console.error(`Error: Unsupported platform: ${platform()}-${arch()}`);
    process.exit(1);
  }

  const binaryPath = findNativeBinary(packageRoot, target);

  if (!binaryPath) {
    console.error(`Error: No binary found for ${platform()}-${arch()}`);
    console.error(`Expected the ${target.packageName} package to be installed.`);
    console.error('');
    console.error('Reinstall agent-browser with optional dependencies enabled,');
    console.error('or run "npm run build:native" to build for your platform.');
    process.exit(1);
  }

  // Ensure binary is executable (fixes EACCES on macOS/Linux when the package
  // manager didn't preserve the execute bit)
  if (platform() !== 'win32') {
    try {
      accessSync(binaryPath, constants.X_OK);
    } catch {
      // Binary exists but isn't executable - fix it
      try {
        chmodSync(binaryPath, 0o755);
      } catch (chmodErr) {
        console.error(`Error: Cannot make binary executable: ${chmodErr.message}`);
        console.error('Try running: chmod +x ' + binaryPath);
        process.exit(1);
      }
    }
  }

  let runPath = binaryPath;
  try {
    const { linkNative, materializeBinary } = await import('../scripts/link-native.js');
    try {
      runPath = materializeBinary(packageRoot, binaryPath, target);
    } catch {
      runPath = binaryPath;
    }
    linkNative({ packageRoot, binary: runPath, target, entries: [process.argv[1]] });
  } catch {
    // Linking is an optimization; this launcher keeps working without it.
  }

  // Spawn the native binary with inherited stdio
  const child = spawn(runPath, process.argv.slice(2), {
    stdio: 'inherit',
    windowsHide: false,
  });

  child.on('error', (err) => {
    console.error(`Error executing binary: ${err.message}`);
    process.exit(1);
  });

  child.on('close', (code) => {
    process.exit(code ?? 0);
  });
}

main();
