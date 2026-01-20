#!/usr/bin/env node
/**
 * Cross-platform entry point for agent-browser CLI
 * This script detects the OS/arch and runs the appropriate native binary
 */

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

function getBinaryName() {
  const platform = os.platform();
  const arch = os.arch();

  let osName;
  switch (platform) {
    case 'darwin':
      osName = 'darwin';
      break;
    case 'linux':
      osName = 'linux';
      break;
    case 'win32':
      osName = 'win32';
      break;
    default:
      console.error(`Unsupported platform: ${platform}`);
      process.exit(1);
  }

  let archName;
  switch (arch) {
    case 'x64':
    case 'amd64':
      archName = 'x64';
      break;
    case 'arm64':
    case 'aarch64':
      archName = 'arm64';
      break;
    default:
      console.error(`Unsupported architecture: ${arch}`);
      process.exit(1);
  }

  const ext = platform === 'win32' ? '.exe' : '';
  return `agent-browser-${osName}-${archName}${ext}`;
}

const binDir = __dirname;
const binaryName = getBinaryName();
const binaryPath = path.join(binDir, binaryName);

if (!fs.existsSync(binaryPath)) {
  console.error(`Error: Binary not found: ${binaryPath}`);
  console.error(`Run 'npm run build:native' to build for your platform`);
  process.exit(1);
}

const child = spawn(binaryPath, process.argv.slice(2), {
  stdio: 'inherit',
  windowsHide: true,
});

child.on('error', (err) => {
  console.error(`Failed to start: ${err.message}`);
  process.exit(1);
});

child.on('close', (code) => {
  process.exit(code ?? 0);
});
