#!/usr/bin/env node

/**
 * Postinstall script for agent-browser
 * 
 * Downloads the platform-specific native binary if not present.
 */

import { existsSync, mkdirSync, chmodSync, createWriteStream, unlinkSync } from 'fs';
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
    console.log(`✓ Native binary already exists: ${binaryName}`);
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

  // Reminder about Playwright browsers
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

main().catch(console.error);
