/**
 * Points package-manager bin entries for agent-browser at the native binary,
 * so later invocations skip the Node.js launcher entirely.
 *
 * Run by postinstall when install scripts are allowed, and by the launcher on
 * its first invocation when they are not (npm 12, pnpm and bun block
 * dependency install scripts by default). Every step is best effort, and an
 * entry is only rewritten when it provably targets this package's launcher.
 */

import {
  chmodSync,
  copyFileSync,
  existsSync,
  linkSync,
  lstatSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'fs';
import { basename, dirname, join, relative, resolve } from 'path';
import { platform } from 'os';

/**
 * Ensures the binary lives in this package's own bin/ directory and returns
 * that path. Bin entries must target files inside the package that owns them:
 * npm only removes such links on uninstall, and bun refuses to replace a
 * global bin entry that points into another package.
 */
export function materializeBinary(packageRoot, binary, target) {
  const binDir = join(packageRoot, 'bin');
  if (dirname(resolve(binary)) === resolve(binDir)) return binary;

  const destination = join(binDir, target.binaryName);
  const staged = `${destination}.install-${process.pid}`;
  try {
    try {
      linkSync(binary, staged);
    } catch {
      copyFileSync(binary, staged);
    }
    if (platform() !== 'win32') chmodSync(staged, 0o755);
    renameSync(staged, destination);
  } finally {
    rmSync(staged, { force: true });
  }
  return destination;
}

/** Writes `content` to `path` atomically, leaving the original intact on failure. */
function replaceFile(path, content, options) {
  const staged = `${path}.install-${process.pid}`;
  try {
    writeFileSync(staged, content, options);
    renameSync(staged, path);
    return true;
  } catch {
    rmSync(staged, { force: true });
    return false;
  }
}

function sameFile(a, b) {
  try {
    return realpathSync(a) === realpathSync(b);
  } catch {
    return false;
  }
}

function isLauncherSymlink(entry, launcher) {
  try {
    return lstatSync(entry).isSymbolicLink() && sameFile(entry, launcher);
  } catch {
    return false;
  }
}

// npm, yarn and bun link bin entries as symlinks on macOS and Linux.
function relinkSymlink(entry, binary) {
  const staged = join(dirname(entry), `.${basename(entry)}-${process.pid}`);
  try {
    symlinkSync(binary, staged);
    renameSync(staged, entry);
    return true;
  } catch {
    rmSync(staged, { force: true });
    return false;
  }
}

function shellQuote(value) {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

/**
 * pnpm writes a shell shim that runs `node <launcher>`, named in a
 * `# cmd-shim-target=` comment. The comment is kept so pnpm still recognizes
 * the shim as its own when it updates or removes the package.
 */
export function pnpmShimPaths(invokedPath) {
  const match = invokedPath.match(
    /^(.*)[\\/]global[\\/][^\\/]+[\\/](?:[^\\/]+[\\/])?node_modules[\\/]agent-browser[\\/]bin[\\/]agent-browser\.js$/,
  );
  if (!match) return [];
  return [join(match[1], 'bin', 'agent-browser'), join(match[1], 'agent-browser')];
}

function relinkPnpmShim(shim, launcher, binary) {
  let content;
  try {
    content = readFileSync(shim, 'utf8');
  } catch {
    return false;
  }
  const declared = content.match(/^# cmd-shim-target=(.+)$/m)?.[1];
  if (!declared || !sameFile(declared, launcher)) return false;
  const marker = `# cmd-shim-target=${declared}`;
  return replaceFile(shim, `#!/bin/sh\n${marker}\nexec ${shellQuote(binary)} "$@"\n`, { mode: 0o755 });
}

/** Returns the launcher path an npm Windows shim runs, or null. */
function windowsShimTarget(shim, shimDir) {
  let content;
  try {
    content = readFileSync(shim, 'utf8');
  } catch {
    return null;
  }
  const patterns = [
    /"%(?:~dp0|dp0%)\\([^"\r\n]+?)"\s+%\*/,
    /"\$basedir[/\\]([^"\r\n]+?)"\s+\$args/,
    /"\$basedir[/\\]([^"\r\n]+?)"\s+"\$@"/,
  ];
  for (const pattern of patterns) {
    const match = content.match(pattern);
    if (match) return resolve(shimDir, match[1].replace(/\\/g, '/'));
  }
  return null;
}

function windowsShimBody(shim, rel) {
  const slash = rel.replace(/\\/g, '/');
  const backslash = rel.replace(/\//g, '\\');
  if (shim.endsWith('.cmd')) return `@ECHO off\r\n"%~dp0\\${backslash}" %*\r\n`;
  if (shim.endsWith('.ps1')) {
    return `#!/usr/bin/env pwsh\r\n$basedir = Split-Path $MyInvocation.MyCommand.Definition -Parent\r\n& "$basedir/${slash}" $args\r\nexit $LASTEXITCODE\r\n`;
  }
  return `#!/bin/sh\nbasedir=$(dirname "$(echo "$0" | sed -e 's,\\\\,/,g')")\nexec "$basedir/${slash}" "$@"\n`;
}

/**
 * Rewrites npm's Windows .cmd/.ps1/sh shims in the global prefix so they run
 * the native binary. Only a shim that resolves to this package's launcher is
 * touched, and each is replaced atomically.
 */
function relinkWindowsShims(packageRoot, linked) {
  const nodeModules = dirname(packageRoot);
  if (basename(packageRoot) !== 'agent-browser' || basename(nodeModules) !== 'node_modules') return false;
  const prefix = dirname(nodeModules);
  const launcher = join(packageRoot, 'bin', 'agent-browser.js');

  let binary;
  let changed = false;
  for (const name of ['agent-browser.cmd', 'agent-browser.ps1', 'agent-browser']) {
    const shim = join(prefix, name);
    const target = windowsShimTarget(shim, prefix);
    if (!target || !sameFile(target, launcher)) continue;
    binary ??= linked();
    changed = replaceFile(shim, windowsShimBody(shim, relative(prefix, binary))) || changed;
  }
  return changed;
}

/**
 * Rewrites every bin entry that runs the launcher so it runs the native
 * binary from this package's own bin/, which keeps skills/, skill-data/ and
 * .install-method reachable from the executable.
 *
 * - packageRoot: this package's directory (real path)
 * - binary: the native binary found by findNativeBinary
 * - target: the nativeTarget for this host
 * - entries: paths that may be symlinks to the launcher, pnpm shims, or the
 *   launcher .js a pnpm shim runs (e.g. process.argv[1])
 *
 * Returns true when at least one entry now runs the native binary.
 */
export function linkNative({ packageRoot, binary, target, entries = [] }) {
  const launcher = join(packageRoot, 'bin', 'agent-browser.js');
  const linked = () => materializeBinary(packageRoot, binary, target);

  if (platform() === 'win32') {
    return relinkWindowsShims(packageRoot, linked);
  }

  let changed = false;
  for (const entry of entries) {
    if (isLauncherSymlink(entry, launcher)) {
      try {
        changed = relinkSymlink(entry, linked()) || changed;
      } catch {
        // Read-only or root-owned install: keep using the launcher.
      }
    }
    for (const shim of [entry, ...pnpmShimPaths(entry)]) {
      try {
        changed = relinkPnpmShim(shim, launcher, realpathSafe(linked())) || changed;
      } catch {
        // Read-only or root-owned install: keep using the launcher.
      }
    }
  }
  return changed;
}

function realpathSafe(path) {
  try {
    return realpathSync(path);
  } catch {
    return path;
  }
}
