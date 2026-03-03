#!/usr/bin/env node

/**
 * Postinstall script for agent-browser
 * 
 * Downloads the platform-specific native binary if not present.
 * On global installs, patches npm's bin entry to use the native binary directly:
 * - Windows: Overwrites .cmd/.ps1 shims
 * - Mac/Linux: Replaces symlink to point to native binary
 */

import { existsSync, mkdirSync, chmodSync, createWriteStream, unlinkSync, writeFileSync, symlinkSync, lstatSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { platform, arch } from 'os';
import { get } from 'https';
import { execSync } from 'child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = join(__dirname, '..');
const binDir = join(projectRoot, 'bin');

// Platform detection
const platformKey = `${platform()}-${arch()}`;
const ext = platform() === 'win32' ? '.exe' : '';
const binaryName = `agent-browser-${platformKey}${ext}`;
const binaryPath = join(binDir, binaryName);

// Package info
const packageJson = JSON.parse(
  (await import('fs')).readFileSync(join(projectRoot, 'package.json'), 'utf8')
);
const version = packageJson.version;

// GitHub release URL
const GITHUB_REPO = 'vercel-labs/agent-browser';
const DOWNLOAD_URL = `https://github.com/${GITHUB_REPO}/releases/download/v${version}/${binaryName}`;

async function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);
    
    const request = (url) => {
      get(url, (response) => {
        // Handle redirects
        if (response.statusCode === 301 || response.statusCode === 302) {
          request(response.headers.location);
          return;
        }
        
        if (response.statusCode !== 200) {
          reject(new Error(`Failed to download: HTTP ${response.statusCode}`));
          return;
        }
        
        response.pipe(file);
        file.on('finish', () => {
          file.close();
          resolve();
        });
      }).on('error', (err) => {
        unlinkSync(dest);
        reject(err);
      });
    };
    
    request(url);
  });
}

async function main() {
  // Check if binary already exists
  if (existsSync(binaryPath)) {
    // Ensure binary is executable (npm doesn't preserve execute bit)
    if (platform() !== 'win32') {
      chmodSync(binaryPath, 0o755);
    }
    console.log(`✓ Native binary ready: ${binaryName}`);
    
    // On global installs, fix npm's bin entry to use native binary directly
    await fixGlobalInstallBin();
    
    showPlaywrightReminder();
    return;
  }

  // Ensure bin directory exists
  if (!existsSync(binDir)) {
    mkdirSync(binDir, { recursive: true });
  }

  console.log(`Downloading native binary for ${platformKey}...`);
  console.log(`URL: ${DOWNLOAD_URL}`);

  try {
    await downloadFile(DOWNLOAD_URL, binaryPath);
    
    // Make executable on Unix
    if (platform() !== 'win32') {
      chmodSync(binaryPath, 0o755);
    }
    
    console.log(`✓ Downloaded native binary: ${binaryName}`);
  } catch (err) {
    console.log(`⚠ Could not download native binary: ${err.message}`);
    console.log(`  The CLI will use Node.js fallback (slightly slower startup)`);
    console.log('');
    console.log('To build the native binary locally:');
    console.log('  1. Install Rust: https://rustup.rs');
    console.log('  2. Run: npm run build:native');
  }

  // On global installs, fix npm's bin entry to use native binary directly
  // This avoids the /bin/sh error on Windows and provides zero-overhead execution
  await fixGlobalInstallBin();

  showPlaywrightReminder();
}

function showPlaywrightReminder() {
  console.log('');
  console.log('╔═══════════════════════════════════════════════════════════════════════════╗');
  console.log('║ To download browser binaries, run:                                        ║');
  console.log('║                                                                           ║');
  console.log('║     npx playwright install chromium                                       ║');
  console.log('║                                                                           ║');
  console.log('║ On Linux, include system dependencies with:                               ║');
  console.log('║                                                                           ║');
  console.log('║     npx playwright install --with-deps chromium                           ║');
  console.log('║                                                                           ║');
  console.log('╚═══════════════════════════════════════════════════════════════════════════╝');
}

/**
 * Fix npm's bin entry on global installs to use the native binary directly.
 * This provides zero-overhead CLI execution for global installs.
 */
async function fixGlobalInstallBin() {
  if (platform() === 'win32') {
    await fixWindowsShims();
  } else {
    await fixUnixSymlink();
  }
}

/**
 * Fix npm symlink on Mac/Linux global installs.
 * Replace the symlink to the JS wrapper with a symlink to the native binary.
 */
async function fixUnixSymlink() {
  // Get npm's global bin directory (npm prefix -g + /bin)
  let npmBinDir;
  try {
    const prefix = execSync('npm prefix -g', { encoding: 'utf8' }).trim();
    npmBinDir = join(prefix, 'bin');
  } catch {
    return; // npm not available
  }

  const symlinkPath = join(npmBinDir, 'agent-browser');

  // Check if symlink exists (indicates global install)
  try {
    const stat = lstatSync(symlinkPath);
    if (!stat.isSymbolicLink()) {
      return; // Not a symlink, don't touch it
    }
  } catch {
    return; // Symlink doesn't exist, not a global install
  }

  // Replace symlink to point directly to native binary
  try {
    unlinkSync(symlinkPath);
    symlinkSync(binaryPath, symlinkPath);
    console.log('✓ Optimized: symlink points to native binary (zero overhead)');
  } catch (err) {
    // Permission error or other issue - not critical, JS wrapper still works
    console.log(`⚠ Could not optimize symlink: ${err.message}`);
    console.log('  CLI will work via Node.js wrapper (slightly slower startup)');
  }
}

/**
 * Fix npm-generated shims on Windows global installs.
 * npm generates shims that try to run /bin/sh, which doesn't exist on Windows.
 * We overwrite them to invoke the native .exe directly.
 */
async function fixWindowsShims() {
  // Check if this is a global install by looking for npm's global prefix
  let npmBinDir;
  try {
    npmBinDir = execSync('npm prefix -g', { encoding: 'utf8' }).trim();
  } catch {
    return; // Not a global install or npm not available
  }

  // The shims are in the npm prefix directory (not prefix/bin on Windows)
  const cmdShim = join(npmBinDir, 'agent-browser.cmd');
  const ps1Shim = join(npmBinDir, 'agent-browser.ps1');

  // Only fix if shims exist (indicates global install)
  if (!existsSync(cmdShim)) {
    return;
  }

  // Path to native binary relative to npm prefix
  const relativeBinaryPath = 'node_modules\\agent-browser\\bin\\agent-browser-win32-x64.exe';

  try {
    // Overwrite .cmd shim
    const cmdContent = `@ECHO off\r\n"%~dp0${relativeBinaryPath}" %*\r\n`;
    writeFileSync(cmdShim, cmdContent);

    // Overwrite .ps1 shim
    const ps1Content = `#!/usr/bin/env pwsh
$basedir = Split-Path $MyInvocation.MyCommand.Definition -Parent
$exe = ""
if ($PSVersionTable.PSVersion -lt "6.0" -or $IsWindows) {
  $exe = ".exe"
}
& "$basedir/${relativeBinaryPath.replace(/\\/g, '/')}" $args
exit $LASTEXITCODE
`;
    writeFileSync(ps1Shim, ps1Content);

    console.log('✓ Optimized: shims point to native binary (zero overhead)');
  } catch (err) {
    // Permission error or other issue - not critical, JS wrapper still works
    console.log(`⚠ Could not optimize shims: ${err.message}`);
    console.log('  CLI will work via Node.js wrapper (slightly slower startup)');
  }
}

main().catch(console.error);
