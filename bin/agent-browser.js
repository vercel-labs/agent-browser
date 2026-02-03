#!/usr/bin/env node

/**
 * Cross-platform CLI wrapper for agent-browser
 * 
 * This wrapper enables npx support on Windows where shell scripts don't work.
 * For global installs, postinstall.js patches the shims to invoke the native
 * binary directly (zero overhead).
 */

import { spawn } from 'child_process';
import { existsSync, accessSync, chmodSync, constants } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { platform, arch, homedir } from 'os';

const __dirname = dirname(fileURLToPath(import.meta.url));

// [Fix] Auto-start daemon on Windows if missing
async function ensureDaemonRunning() {
  if (platform() !== 'win32') return;

  const agentDir = join(homedir(), '.agent-browser');
  const portFile = join(agentDir, 'default.port');

  if (existsSync(portFile)) return;

  // console.log('[Auto-Fix] Starting agent-browser daemon...');
  // The daemon is located in ../dist/daemon.js relative to this script
  const daemonScript = join(__dirname, '../dist/daemon.js');
  
  if (!existsSync(daemonScript)) {
     // If dist doesn't exist (e.g. dev mode), try src (via tsx maybe?) 
     // but in production/installed package dist should exist.
     return; 
  }

  try {
    const child = spawn(process.execPath, [daemonScript], {
      detached: true,
      stdio: 'ignore',
      windowsHide: true
    });
    child.unref();

    // Wait up to 3s for startup
    const start = Date.now();
    while (Date.now() - start < 3000) {
      if (existsSync(portFile)) break;
      await new Promise(r => setTimeout(r, 100));
    }
  } catch (e) {
    // Ignore spawn errors, let the native binary try its own startup
  }
}

// Map Node.js platform/arch to binary naming convention
function getBinaryName() {
  const os = platform();
  const cpuArch = arch();

  let osKey;
  switch (os) {
    case 'darwin':
      osKey = 'darwin';
      break;
    case 'linux':
      osKey = 'linux';
      break;
    case 'win32':
      osKey = 'win32';
      break;
    default:
      return null;
  }

  let archKey;
  switch (cpuArch) {
    case 'x64':
    case 'x86_64':
      archKey = 'x64';
      break;
    case 'arm64':
    case 'aarch64':
      archKey = 'arm64';
      break;
    default:
      return null;
  }

  const ext = os === 'win32' ? '.exe' : '';
  return `agent-browser-${osKey}-${archKey}${ext}`;
}

async function main() {
  await ensureDaemonRunning();

  const binaryName = getBinaryName();

  if (!binaryName) {
    console.error(`Error: Unsupported platform: ${platform()}-${arch()}`);
    process.exit(1);
  }

  const binaryPath = join(__dirname, binaryName);

  // If native binary is missing, we could try to run the Node.js version directly?
  // But for now let's stick to the original logic which expects the binary.
  
  if (!existsSync(binaryPath)) {
    console.error(`Error: No binary found for ${platform()}-${arch()}`);
    console.error(`Expected: ${binaryPath}`);
    console.error('');
    console.error('Run "npm run build:native" to build for your platform,');
    console.error('or reinstall the package to trigger the postinstall download.');
    process.exit(1);
  }

  // Ensure binary is executable (fixes EACCES on macOS/Linux when postinstall didn't run,
  // e.g., when using bun which blocks lifecycle scripts by default)
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

  // Spawn the native binary with inherited stdio
  const child = spawn(binaryPath, process.argv.slice(2), {
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
