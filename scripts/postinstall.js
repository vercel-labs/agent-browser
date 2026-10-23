#!/usr/bin/env node

/**
 * Postinstall script for agent-browser
 *
 * Finds the native binary from the platform package (an optional dependency),
 * downloading it from the GitHub release only when that package is missing.
 * Then points the package manager's bin entry at the native binary:
 * - Windows: Overwrites npm's .cmd/.ps1 shims
 * - Mac/Linux: Replaces the npm/yarn/bun symlink or the pnpm shim
 *
 * Install scripts are blocked by default in npm 12, pnpm and bun; the launcher
 * (bin/agent-browser.js) then performs the same linking on its first run.
 */

import { existsSync, mkdirSync, chmodSync, createWriteStream, realpathSync, unlinkSync, writeFileSync } from 'fs';
import { basename, dirname, isAbsolute, join, sep } from 'path';
import { fileURLToPath } from 'url';
import { platform, arch, tmpdir } from 'os';
import { get } from 'https';
import { execSync } from 'child_process';
import { findNativeBinary, nativeTarget } from './native.js';
import { linkNative } from './link-native.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = join(__dirname, '..');
const binDir = join(projectRoot, 'bin');

const target = nativeTarget();
// Windows ARM64 falls back to x64 binary (no native ARM64 build available).
// x64 binaries run via Windows' built-in emulation on ARM64.
const binaryName = target?.binaryName ?? `agent-browser-${platform()}-${arch()}`;
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

/**
 * Detect which package manager ran this postinstall and write a marker file
 * next to the binary so `agent-browser upgrade` can use the correct one
 * without fragile path heuristics or slow subprocess probing.
 *
 * npm_config_user_agent is set by npm/pnpm/yarn/bun during lifecycle scripts,
 * e.g. "pnpm/8.10.0 node/v20.10.0 linux x64"
 */
function writeInstallMethod() {
  const method = detectManager();

  if (method) {
    try {
      writeFileSync(join(binDir, '.install-method'), method);
    } catch {
      // Non-critical — upgrade will fall back to heuristics
    }
  }
}

async function main() {
  if (!target) {
    console.log(`No native binary available for ${platform()}-${arch()}`);
    return;
  }

  let binary = findNativeBinary(projectRoot, target);

  if (binary) {
    // Ensure binary is executable (npm doesn't preserve execute bit)
    if (platform() !== 'win32') {
      try {
        chmodSync(binary, 0o755);
      } catch {
        // Read-only package store; the launcher retries before running it
      }
    }
    console.log(`✓ Native binary ready: ${dirname(binary) === binDir ? binaryName : target.packageName}`);
  } else {
    // The platform package is missing, e.g. --omit=optional or a lockfile
    // created on another platform. Fall back to the GitHub release asset.
    if (!existsSync(binDir)) {
      mkdirSync(binDir, { recursive: true });
    }

    console.log(`Downloading native binary for ${platform()}-${arch()}...`);
    if (platform() === 'win32' && arch() === 'arm64') {
      console.log(`  Note: Using x64 binary on ARM64 Windows (runs via emulation)`);
    }
    console.log(`URL: ${DOWNLOAD_URL}`);

    try {
      await downloadFile(DOWNLOAD_URL, binaryPath);

      // Make executable on Unix
      if (platform() !== 'win32') {
        chmodSync(binaryPath, 0o755);
      }

      console.log(`✓ Downloaded native binary: ${binaryName}`);
      binary = binaryPath;
    } catch (err) {
      console.log(`Could not download native binary: ${err.message}`);
      console.log('');
      console.log('To build the native binary locally:');
      console.log('  1. Install Rust: https://rustup.rs');
      console.log('  2. Run: npm run build:native');
    }
  }

  writeInstallMethod();

  // On global installs, point the bin entry at the native binary directly.
  // This avoids the /bin/sh error on Windows and provides zero-overhead execution.
  if (binary) {
    fixGlobalInstallBin(binary);
  }

  showInstallReminder();
}

function findSystemChrome() {
  const os = platform();
  if (os === 'darwin') {
    const candidates = [
      '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      '/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary',
      '/Applications/Chromium.app/Contents/MacOS/Chromium',
    ];
    return candidates.find(p => existsSync(p)) || null;
  }
  if (os === 'linux') {
    const names = ['google-chrome', 'google-chrome-stable', 'chromium-browser', 'chromium'];
    for (const name of names) {
      try {
        const result = execSync(`which ${name} 2>/dev/null`, { encoding: 'utf8' }).trim();
        if (result) return result;
      } catch {}
    }
    return null;
  }
  if (os === 'win32') {
    const candidates = [
      `${process.env.LOCALAPPDATA}\\Google\\Chrome\\Application\\chrome.exe`,
      'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
      'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
    ];
    return candidates.find(p => p && existsSync(p)) || null;
  }
  return null;
}

function showInstallReminder() {
  const systemChrome = findSystemChrome();
  if (systemChrome) {
    console.log('');
    console.log(`  ✓ System Chrome found: ${systemChrome}`);
    console.log('    agent-browser will use it automatically.');
    console.log('');
    return;
  }

  console.log('');
  console.log('  ⚠ No Chrome installation detected.');
  console.log('  If you plan to use a local browser, run:');
  console.log('');
  console.log('    agent-browser install');
  if (platform() === 'linux') {
    console.log('');
    console.log('  On Linux, include system dependencies with:');
    console.log('');
    console.log('    agent-browser install --with-deps');
  }
  console.log('');
  console.log('  You can skip this if you use --cdp, --provider, --engine, or --executable-path.');
  console.log('');
}

/** Package manager running this install, from its user agent string. */
function detectManager() {
  const ua = process.env.npm_config_user_agent || '';
  return ['pnpm', 'yarn', 'bun', 'npm'].find((name) => ua.startsWith(`${name}/`)) || '';
}

/** A manager's global bin directory sits at the prefix itself on Windows. */
function prefixToBin(prefix) {
  return platform() === 'win32' ? prefix : join(prefix, 'bin');
}

/**
 * True when this package is being installed globally. Only npm reports it
 * directly; the others are recognised by the global store their files land in.
 */
function isGlobalInstall() {
  if (process.env.npm_config_global === 'true') return true;
  const root = realpathOrSelf(projectRoot);
  for (const dir of [process.env.PNPM_HOME, process.env.BUN_INSTALL_GLOBAL_DIR, process.env.YARN_GLOBAL_FOLDER]) {
    if (dir && root.startsWith(realpathOrSelf(dir))) return true;
  }
  return root.includes(`${sep}global${sep}`);
}

function realpathOrSelf(path) {
  try {
    return realpathSync(path);
  } catch {
    return path;
  }
}

/**
 * Global bin directories that may hold this package's bin entry: the prefix
 * npm reports, the invoking manager's own answer, and the enclosing
 * node_modules/.bin that Yarn Classic links through before its global link
 * exists. linkNative only rewrites an entry resolving to this package.
 */
function globalBinCandidates() {
  const candidates = [];
  const prefix = process.env.npm_config_prefix;
  if (prefix) {
    candidates.push(prefixToBin(prefix));
  }

  const probes = {
    pnpm: 'pnpm bin -g',
    yarn: 'yarn global bin',
    bun: 'bun pm bin -g',
    npm: 'npm prefix -g',
  };
  const manager = detectManager();
  const needsProbe = probes[manager] && isGlobalInstall() && !(manager === 'npm' && prefix);
  if (needsProbe) {
    try {
      const out = execSync(probes[manager], {
        encoding: 'utf8',
        cwd: tmpdir(),
        timeout: 5000,
        stdio: ['ignore', 'pipe', 'ignore'],
      });
      const line = out.split(/\r?\n/).map((l) => l.trim()).filter(Boolean).pop();
      if (line && isAbsolute(line)) {
        candidates.push(manager === 'npm' ? prefixToBin(line) : line);
      }
    } catch {
      return candidates;
    }
  }

  const parent = join(projectRoot, '..');
  if (basename(parent) === 'node_modules') {
    candidates.push(join(parent, '.bin'));
  }

  return [...new Set(candidates)];
}

/** Points the global bin entry at the native binary, skipping Node entirely. */
function fixGlobalInstallBin(binary) {
  const entries = globalBinCandidates().map((dir) => join(dir, 'agent-browser'));

  try {
    if (linkNative({ packageRoot: projectRoot, binary, target, entries })) {
      console.log('✓ Optimized: bin entry points to native binary (zero overhead)');
    }
  } catch (err) {
    // Permission error or other issue - not critical, the launcher links on first run
    console.log(`⚠ Could not optimize bin entry: ${err.message}`);
    console.log('  CLI will work via Node.js wrapper until its first run links the native binary');
  }
}

main().catch(console.error);
